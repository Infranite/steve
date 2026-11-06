//! B1: the registries between modules and steve-core (hooks, prompt context, panels, disclosure check,
//! decision undo), the scripted gateway and the clock.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::context::{ChatCtx, ChatDone, Cite, PromptContext};
use steve_modules::decisions::{DecisionCx, DecisionHandler};
use steve_modules::hooks::{ItemHooks, Takeover};
use steve_modules::outbound::{
    self, Audience, CheckCx, CheckResult, KnowledgeUse, OutboundCheck, Verdict,
};
use steve_modules::panels::{self, ActionOutcome, ItemPanel, Panel};
use steve_modules::testkit::{ME, TestEnv, WS, caller};
use steve_modules::{
    ApiError, ApiResult, Caller, Clock, FakeClock, Gateway, GatewayOut, Modules, Registry,
    SystemClock, Tier,
};
use steve_store::base::{Level, Viewer};
use steve_store::{Decision, Item, NewBot, NewDecision, NewItem, Tx};

type Log = Arc<Mutex<Vec<String>>>;

fn item(t: &TestEnv) -> Item {
    let s = t.store();
    s.bots()
        .seed(
            "steve",
            true,
            &NewBot {
                name: "Steve".into(),
                harness: "claude-code".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.items()
        .create(
            WS,
            &NewItem {
                title: "修 bug".into(),
                goal: "g".into(),
                bot_id: "steve".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
}

struct Hook {
    name: &'static str,
    take: bool,
    log: Log,
}

#[async_trait]
impl ItemHooks for Hook {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn on_created(&self, i: &Item) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} created {}", self.name, i.id));
    }
    async fn on_accepted(&self, _: &Item) -> Option<Takeover> {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} accepted", self.name));
        self.take.then_some(Takeover { by: self.name })
    }
    async fn on_finished(&self, _: &Item) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} finished", self.name));
    }
    // on_dropped: default no-op
}

fn hook(name: &'static str, take: bool, log: &Log) -> Box<Hook> {
    Box::new(Hook {
        name,
        take,
        log: log.clone(),
    })
}

#[tokio::test]
async fn item_hooks_run_in_order_and_only_the_first_takeover_counts() {
    let log = Log::default();
    let l = log.clone();
    let t = TestEnv::new(move |r| {
        r.item_hooks(hook("a", false, &l))
            .item_hooks(hook("b", true, &l))
            .item_hooks(hook("c", true, &l));
    });
    let i = item(&t);
    let hooks = t.modules.registry.hooks();
    hooks.created(&i).await;
    assert_eq!(hooks.accepted(&i).await, Some(Takeover { by: "b" }));
    hooks.finished(&i).await;
    hooks.dropped(&i).await;
    assert_eq!(
        *log.lock().unwrap(),
        vec![
            format!("a created {}", i.id),
            format!("b created {}", i.id),
            format!("c created {}", i.id),
            "a accepted".into(),
            "b accepted".into(), // c is never asked
            "a finished".into(),
            "b finished".into(),
            "c finished".into(),
        ]
    );
}

#[tokio::test]
async fn nothing_registered_is_a_no_op() {
    let t = TestEnv::new(|_| {});
    let i = item(&t);
    let r = &t.modules.registry;
    assert!(r.hooks().is_empty());
    r.hooks().created(&i).await;
    assert_eq!(r.hooks().accepted(&i).await, None);
    r.hooks().finished(&i).await;
    r.hooks().dropped(&i).await;
    t.store()
        .reader(|c| {
            assert_eq!(r.contexts().session(c, WS, &i), "");
            assert!(
                r.contexts()
                    .chat(c, WS, &Viewer::bare(WS, ME), "hi")
                    .is_none()
            );
            assert!(panels::panels(&t.modules, c, WS, ME, &i.id)?.is_empty());
            Ok::<_, ApiError>(())
        })
        .unwrap();
    let err = panels::action(
        &t.modules,
        &caller(WS, ME),
        &i.id,
        "github.pr:ghp_1",
        "open",
    )
    .await
    .unwrap_err();
    assert_eq!(err, ApiError::not_found("面板"));
    let res = outbound::check(
        &t.modules,
        &CheckCx::default(),
        &Audience::default(),
        "x",
        &[],
    )
    .await;
    assert_eq!(res, CheckResult::unchecked());
    assert_eq!(
        serde_json::to_value(&res).unwrap(),
        json!({"verdict": "unchecked", "entries": [], "spans": []})
    );
}

#[test]
#[should_panic(expected = "item hooks a registered twice")]
fn hooks_with_the_same_name_panic() {
    let log = Log::default();
    let mut r = Registry::new();
    r.item_hooks(hook("a", false, &log))
        .item_hooks(hook("a", false, &log));
}

struct Ctx(&'static str);

impl PromptContext for Ctx {
    fn session(&self, _: &Connection, _: &str, item: &Item) -> Option<String> {
        (!self.0.is_empty()).then(|| format!("{} · {}", self.0, item.title))
    }
    fn chat(&self, _: &Connection, _: &str, v: &Viewer, text: &str) -> Option<ChatCtx> {
        (!self.0.is_empty()).then(|| ChatCtx {
            text: format!("{}: {text} for {}", self.0, v.user),
            cites: vec![Cite {
                n: 1,
                label: "笔记 · Acme".into(),
                href: "/kb/docs/kd_1".into(),
                level: Level::Public,
            }],
        })
    }
    fn chat_done(
        &self,
        _: &Connection,
        _: &str,
        mid: &str,
        answer: &str,
        cx: &ChatCtx,
    ) -> ChatDone {
        let mut meta = serde_json::Map::new();
        meta.insert("cites".into(), serde_json::to_value(&cx.cites).unwrap());
        meta.insert("mid".into(), Value::String(mid.into()));
        ChatDone {
            text: answer.replace(" [9]", ""),
            meta: Some(meta),
        }
    }
}

#[test]
fn prompt_contexts_join_sessions_and_take_the_first_chat() {
    let t = TestEnv::new(|r| {
        r.prompt_context(Box::new(Ctx("")))
            .prompt_context(Box::new(Ctx("知识")))
            .prompt_context(Box::new(Ctx("卡片")));
    });
    let i = item(&t);
    let cx = t.modules.registry.contexts();
    t.store()
        .reader(|c| {
            assert_eq!(cx.session(c, WS, &i), "知识 · 修 bug\n\n卡片 · 修 bug");
            let (n, chat) = cx.chat(c, WS, &Viewer::bare(WS, ME), "合同").unwrap();
            assert_eq!(n, 1);
            assert_eq!(chat.text, "知识: 合同 for usr_dev");
            let done = cx.chat_done(n, c, WS, "msg_1", "好的 [1] [9]", &chat);
            assert_eq!(done.text, "好的 [1]");
            assert_eq!(
                Value::Object(done.meta.unwrap()),
                json!({"cites": [{"n": 1, "label": "笔记 · Acme", "href": "/kb/docs/kd_1", "level": "public"}], "mid": "msg_1"})
            );
            // an unknown provider index leaves the answer alone
            assert_eq!(cx.chat_done(9, c, WS, "m", "a", &chat), ChatDone { text: "a".into(), meta: None });
            Ok::<_, ApiError>(())
        })
        .unwrap();
}

/// The GitHub panel example, with the alternatives resolved.
fn github_panel_json() -> Value {
    json!({"id":"github.pr:ghp_1","plugin":"github","title":"PR 与检查",
     "head":{"text":"#12 fix: preserve return_to","href":"https://github.com/o/r/pull/12","badges":["草稿"]},
     "rows":[{"state":"ok","text":"build","sub":"2 分钟前","href":"https://github.com/o/r/runs/1"},
             {"state":"fail","text":"lint"},{"state":"pending","text":"e2e"},{"state":"skipped","text":"deploy"}],
     "foot":"3 项检查通过 · 等你验收后合并",
     "actions":[{"id":"rerun_failed","label":"重跑失败的检查"},{"id":"open","label":"在 GitHub 打开","href":"https://github.com/o/r/pull/12"}],
     "updated":0})
}

#[test]
fn panel_json_has_the_github_md_shape() {
    let v = github_panel_json();
    let p: Panel = serde_json::from_value(v.clone()).unwrap();
    assert_eq!(serde_json::to_value(&p).unwrap(), v);
    // optional parts are left out, lists are always there
    let bare: Panel =
        serde_json::from_value(json!({"id":"x:1","plugin":"x","title":"t","updated":5})).unwrap();
    assert_eq!(
        serde_json::to_value(&bare).unwrap(),
        json!({"id":"x:1","plugin":"x","title":"t","rows":[],"actions":[],"updated":5})
    );
    assert!(serde_json::from_value::<Panel>(json!({"id":"x","plugin":"x","title":"t","updated":0,"rows":[{"state":"weird","text":"a"}]})).is_err());
    assert_eq!(panels::provider_of("github.pr:ghp_1"), "github");
    assert_eq!(panels::provider_of("mail:mt_1"), "mail");
    assert_eq!(panels::provider_of("plain"), "plain");
}

struct Gh(Log);

#[async_trait]
impl ItemPanel for Gh {
    fn id(&self) -> &'static str {
        "github"
    }
    fn panels(&self, _: &Connection, ws: &str, _: &str, item_id: &str) -> ApiResult<Vec<Panel>> {
        assert_eq!(ws, WS);
        let mut p: Panel = serde_json::from_value(github_panel_json())?;
        p.title = format!("PR 与检查 {item_id}");
        Ok(vec![p])
    }
    async fn action(
        &self,
        m: &Modules,
        c: &Caller,
        item_id: &str,
        panel: &str,
        action: &str,
    ) -> ApiResult<ActionOutcome> {
        let _ = m;
        self.0
            .lock()
            .unwrap()
            .push(format!("{} {item_id} {panel} {action}", c.me));
        match action {
            "rerun_failed" => Ok(ActionOutcome {
                message: Some("已重跑".into()),
            }),
            _ => Err(ApiError::not_found("操作")),
        }
    }
}

struct Mail;

#[async_trait]
impl ItemPanel for Mail {
    fn id(&self) -> &'static str {
        "mail"
    }
    fn panels(&self, _: &Connection, _: &str, _: &str, _: &str) -> ApiResult<Vec<Panel>> {
        Ok(vec![serde_json::from_value(
            json!({"id":"mail.thread:mt_1","plugin":"mail","title":"邮件往来","updated":1}),
        )?])
    }
    async fn action(
        &self,
        _: &Modules,
        _: &Caller,
        _: &str,
        _: &str,
        _: &str,
    ) -> ApiResult<ActionOutcome> {
        Ok(ActionOutcome::default())
    }
}

#[tokio::test]
async fn panels_list_in_order_and_actions_go_to_their_provider() {
    let log = Log::default();
    let l = log.clone();
    let t = TestEnv::new(move |r| {
        r.item_panel(Box::new(Gh(l))).item_panel(Box::new(Mail));
    });
    let i = item(&t);
    let ps = t
        .store()
        .reader(|c| panels::panels(&t.modules, c, WS, ME, &i.id))
        .unwrap();
    let ids: Vec<_> = ps.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["github.pr:ghp_1", "mail.thread:mt_1"]);
    assert_eq!(ps[0].title, format!("PR 与检查 {}", i.id));
    let me = caller(WS, ME);
    let out = panels::action(&t.modules, &me, &i.id, "github.pr:ghp_1", "rerun_failed")
        .await
        .unwrap();
    assert_eq!(
        serde_json::to_value(&out).unwrap(),
        json!({"message": "已重跑"})
    );
    assert_eq!(
        panels::action(&t.modules, &me, &i.id, "github.pr:ghp_1", "nope").await,
        Err(ApiError::not_found("操作"))
    );
    assert_eq!(
        serde_json::to_value(
            panels::action(&t.modules, &me, &i.id, "mail.thread:mt_1", "x")
                .await
                .unwrap()
        )
        .unwrap(),
        json!({})
    );
    assert_eq!(
        panels::action(&t.modules, &me, &i.id, "slack.x:1", "x").await,
        Err(ApiError::not_found("面板"))
    );
    assert_eq!(log.lock().unwrap().len(), 2);
}

struct Check;

#[async_trait]
impl OutboundCheck for Check {
    async fn check(
        &self,
        cx: &CheckCx,
        a: &Audience,
        text: &str,
        k: &[KnowledgeUse],
    ) -> CheckResult {
        let leak = a.external
            && k.iter()
                .any(|k| k.level != Level::Public && text.contains(&k.text));
        CheckResult {
            verdict: if leak { Verdict::Leak } else { Verdict::Pass },
            entries: Vec::new(),
            spans: Vec::new(),
            check_model: Some(format!("check-for-{}", cx.actor)),
        }
    }
}

#[tokio::test]
async fn the_registered_outbound_check_is_used() {
    let t = TestEnv::new(|r| {
        r.outbound_check(Box::new(Check));
    });
    let k = [KnowledgeUse {
        source_type: "mem".into(),
        source_id: "mm_1".into(),
        source_label: "记忆".into(),
        level: Level::Reasoning,
        visibility: "private".into(),
        text: "预算 30 万".into(),
    }];
    let cx = CheckCx {
        ws: WS.into(),
        actor: "steve".into(),
        draft_model: None,
        audience_said: None,
        skip_model: false,
    };
    let ext = Audience {
        external: true,
        ..Default::default()
    };
    let r = outbound::check(&t.modules, &cx, &ext, "我们的预算 30 万", &k).await;
    assert_eq!(r.verdict, Verdict::Leak);
    assert_eq!(r.check_model.as_deref(), Some("check-for-steve"));
    let r = outbound::check(
        &t.modules,
        &cx,
        &Audience::default(),
        "我们的预算 30 万",
        &k,
    )
    .await;
    assert_eq!(serde_json::to_value(r.verdict).unwrap(), json!("pass"));
}

struct Undo(Log, bool);

impl DecisionHandler for Undo {
    fn resolve(&self, _: &Tx, cx: &DecisionCx, _: &Decision) -> ApiResult<Value> {
        Ok(json!({"choice": cx.choice}))
    }
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        self.0.lock().unwrap().push(d.id.clone());
        tx.execute("UPDATE parties SET note = 'undone' WHERE 0", [])?;
        if self.1 {
            Err(ApiError::conflict("已经发出，不能撤销"))
        } else {
            Ok(())
        }
    }
}

fn decision(t: &TestEnv, subject_type: &str) -> Decision {
    t.store()
        .decisions()
        .create(&NewDecision {
            workspace_id: WS.into(),
            kind: "approve".into(),
            subject_type: subject_type.into(),
            subject_id: "x_1".into(),
            question: "q".into(),
            requested_by: "steve".into(),
            ..Default::default()
        })
        .unwrap()
        .unwrap()
}

#[test]
fn before_undo_goes_to_the_subject_types_handler() {
    let log = Log::default();
    let (l1, l2) = (log.clone(), log.clone());
    let t = TestEnv::new(move |r| {
        r.decision("ok_undo", Box::new(Undo(l1, false)))
            .decision("no_undo", Box::new(Undo(l2, true)));
    });
    let (a, b, c) = (
        decision(&t, "ok_undo"),
        decision(&t, "no_undo"),
        decision(&t, "item"),
    );
    let r = &t.modules.registry;
    let run = |d: &Decision| t.store().transaction(|tx| r.before_undo(tx, d));
    assert_eq!(run(&a), Ok(()));
    assert_eq!(run(&b), Err(ApiError::conflict("已经发出，不能撤销")));
    assert_eq!(
        run(&c),
        Ok(()),
        "unregistered subject types are left to steve-core"
    );
    assert_eq!(*log.lock().unwrap(), vec![a.id, b.id]);
}

#[tokio::test]
async fn fake_gateway_scripts_by_prefix_and_records_prompts() {
    let t = TestEnv::new(|_| {});
    let g = &t.gateway;
    g.script("分诊：", r#"{"need":true}"#);
    g.script_out(
        "检查：",
        GatewayOut {
            text: "pass".into(),
            cost_usd: Some(0.01),
            model: "haiku".into(),
            ..GatewayOut::default()
        },
    );
    g.script_fail("坏：", "timeout");
    g.reply("queued");
    let d = Duration::from_secs(1);
    let gw: &dyn Gateway = &**g;
    assert_eq!(
        gw.run(Tier::Fast, "分诊：邮件 1", d).await.unwrap().text,
        r#"{"need":true}"#
    );
    assert_eq!(
        gw.run(Tier::Fast, "分诊：邮件 2", d).await.unwrap().text,
        r#"{"need":true}"#,
        "scripts are reusable"
    );
    let o = gw.run(Tier::Fast, "检查：回复", d).await.unwrap();
    assert_eq!((o.cost_usd, o.model.as_str()), (Some(0.01), "haiku"));
    assert_eq!(gw.run(Tier::Smart, "坏：x", d).await, Err("timeout".into()));
    assert_eq!(gw.smart("起草", d).await.unwrap(), "queued");
    assert_eq!(gw.smart("起草", d).await.unwrap(), "{}", "nothing queued");
    assert_eq!(g.last_prompt().as_deref(), Some("起草"));
    let calls = g.calls.lock().unwrap();
    assert_eq!(calls.len(), 6);
    assert_eq!(calls[0], (Tier::Fast, "分诊：邮件 1".into()));
    assert_eq!(calls[4].0, Tier::Smart, "smart() is the smart tier");
    assert_eq!(serde_json::to_value(Tier::Fast).unwrap(), json!("fast"));
}

#[test]
fn clocks() {
    let c = FakeClock::new(1_000);
    assert_eq!(c.now(), 1_000);
    assert_eq!(c.advance(500), 1_500);
    c.set(42);
    assert_eq!(c.now(), 42);
    let before = steve_store::now();
    let s = SystemClock.now();
    assert!(s >= before && s <= steve_store::now());
    // the test env's modules run on its fake clock
    let t = TestEnv::new(|_| {});
    t.clock.set(7);
    assert_eq!(t.modules.clock.now(), 7);
}

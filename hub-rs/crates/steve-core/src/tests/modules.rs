//! Module observers, prompt context, decision undo and the shared model gateway.

use crate::error::msg;
use crate::gateway::{ModelGateway, Printed, SharedGateway};
use crate::repo::{Item, WS};
use crate::testkit::Kit;
use crate::{CreateItem, ItemAction, Tier};
use async_trait::async_trait;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_modules::context::{ChatCtx, ChatDone, PromptContext, SessionPrompt};
use steve_modules::decisions::{DecisionCx, DecisionHandler, DecisionOutcome};
use steve_modules::hooks::ItemHooks;
use steve_modules::{ApiError, ApiResult, Gateway, Modules, Registry};
use steve_store::base::Viewer;
use steve_store::{Decision, W};

type Log = Arc<Mutex<Vec<String>>>;

fn attach(k: &Kit, f: impl FnOnce(&mut Registry)) {
    let mut r = Registry::new();
    k.core.register_change_ops(&mut r);
    f(&mut r);
    let m = Modules::new(k.db().store().clone(), k.core.module_gateway(), r);
    k.core.attach_modules(m);
}

struct Hook {
    name: &'static str,
    log: Log,
}

impl Hook {
    fn new(name: &'static str, log: &Log) -> Box<Self> {
        Box::new(Self {
            name,
            log: log.clone(),
        })
    }
    fn note(&self, what: &str, i: &Item) {
        self.log.lock().unwrap().push(format!(
            "{} {what} {} {}",
            self.name,
            i.id,
            i.phase.as_str()
        ));
    }
}

#[async_trait]
impl ItemHooks for Hook {
    fn name(&self) -> &'static str {
        self.name
    }
    async fn on_created(&self, i: &Item) {
        self.note("created", i);
    }
    async fn on_finished(&self, i: &Item) {
        self.note("finished", i);
    }
    async fn on_dropped(&self, i: &Item) {
        self.note("dropped", i);
    }
}

fn logged(log: &Log) -> Vec<String> {
    log.lock().unwrap().clone()
}

/// Return the observed runtime log after its expected entries have committed.
async fn wait_log(log: &Log, n: usize) -> Vec<String> {
    steve_runtime::testkit::eventually_sync("runtime log reaches its expected entry count", || {
        log.lock().unwrap().len() >= n
    })
    .await;
    logged(log)
}

fn new_item(k: &Kit, goal: &str) -> Item {
    k.core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: goal.into(),
            title: "标题".into(),
            ..Default::default()
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn created_and_dropped_reach_every_hook_in_registration_order() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", &log))
            .item_hooks(Hook::new("b", &log));
    });
    let i = new_item(&k, "写一首诗");
    assert_eq!(
        wait_log(&log, 2).await,
        ["a created ST-1 active", "b created ST-1 active"]
    );
    k.core.item_action(&i.id, ItemAction::Drop, "me").unwrap();
    // the cancel stops the running turn; the close (and the hooks) follow its end
    let sid = k.db().sessions_of_item(&i.id).unwrap()[0].id.clone();
    steve_runtime::testkit::eventually_sync(
        "The fake runtime records cancellation of the target session.",
        || k.rt.st().cancelled.contains(&sid),
    )
    .await;
    k.rt.end_turn(&k.core, &sid, "cancelled", None);
    assert_eq!(
        wait_log(&log, 4).await[2..],
        ["a dropped ST-1 closed", "b dropped ST-1 closed"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proposal_declined_is_a_drop() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", &log));
    });
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "t".into(),
            propose: true,
            ..Default::default()
        })
        .unwrap();
    let d = k.db().decisions_of_item(&i.id).unwrap().remove(0);
    k.core.resolve(&d.id, "no", "", "me").unwrap();
    assert_eq!(
        wait_log(&log, 2).await,
        ["a created ST-1 ready", "a dropped ST-1 closed"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acceptance_commits_before_finished_hooks_observe_it() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", &log))
            .item_hooks(Hook::new("b", &log));
    });
    let delivered = k.delivered(false);
    k.core
        .resolve(&delivered.decision, "pass", "", "me")
        .unwrap();
    assert_eq!(k.phase(&delivered.id), "closed");
    assert_eq!(
        wait_log(&log, 2).await,
        [
            format!("a finished {} closed", delivered.id),
            format!("b finished {} closed", delivered.id),
        ]
    );
}

struct Ctx {
    session: &'static str,
    chat: &'static str,
    seen: Log,
}

impl PromptContext for Ctx {
    fn session(&self, _: &steve_store::Store, ws: &str, item: &Item) -> Option<SessionPrompt> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("session {ws} {}", item.id));
        Some(SessionPrompt {
            user_context: String::new(),
            task_context: self.session.into(),
        })
    }
    fn chat(&self, _: &steve_store::Store, ws: &str, v: &Viewer, text: &str) -> Option<ChatCtx> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("chat {ws} {} {text}", v.user));
        Some(ChatCtx {
            text: self.chat.into(),
            cites: vec![],
        })
    }
    fn chat_done(
        &self,
        _: &steve_store::Store,
        _: &str,
        message_id: &str,
        answer: &str,
        cx: &ChatCtx,
    ) -> ChatDone {
        self.seen
            .lock()
            .unwrap()
            .push(format!("done {message_id} {answer} {}", cx.text));
        let mut meta = serde_json::Map::new();
        meta.insert("cites".into(), json!([{"n": 1}]));
        ChatDone {
            text: answer.replace(" [9]", ""),
            meta: Some(meta),
        }
    }
}

#[test]
fn session_context_goes_into_the_protocol_before_the_steve_tools() {
    let k = Kit::new();
    let seen = Log::default();
    let s2 = seen.clone();
    attach(&k, move |r| {
        r.prompt_context(Box::new(Ctx {
            session: "",
            chat: "",
            seen: s2.clone(),
        }))
        .prompt_context(Box::new(Ctx {
            session: "相关知识：Acme 用 Rust。",
            chat: "",
            seen: s2,
        }));
    });
    let i = new_item(&k, "写一首诗");
    let s = k.db().sessions_of_item(&i.id).unwrap().remove(0);
    let p = &k.rt.prompts_of(&s.id)[0];
    assert!(
        p.contains("\n相关知识：Acme 用 Rust。\nHow to work with Steve — use the steve MCP tools:"),
        "{p}"
    );
    assert!(!p.contains("\n\n"));
    assert_eq!(
        logged(&seen),
        [
            format!("session {WS} {}", i.id),
            format!("session {WS} {}", i.id)
        ]
    );
}

#[test]
fn declarative_append_system_context_routes_user_context_natively_or_to_protocol() {
    struct UserCtxProvider(&'static str, &'static str);
    impl PromptContext for UserCtxProvider {
        fn session(&self, _: &steve_store::Store, _: &str, _: &Item) -> Option<SessionPrompt> {
            Some(SessionPrompt {
                user_context: self.0.into(),
                task_context: self.1.into(),
            })
        }
    }

    let native_harness = steve_harness_spec::builtins()
        .iter()
        .find(|b| b.properties.append_system_context.is_some())
        .expect("at least one builtin declares append_system_context")
        .id;
    let fallback_harness = steve_harness_spec::builtins()
        .iter()
        .find(|b| b.properties.append_system_context.is_none())
        .expect("at least one builtin omits append_system_context")
        .id;

    let k = Kit::new();
    attach(&k, move |r| {
        r.prompt_context(Box::new(UserCtxProvider(
            "Me: run cargo check before every commit.",
            "Task knowledge: SQLite WAL mode.",
        )));
    });

    // Native harness receives user_context via OpenSession.system_context, not duplicated in protocol.
    let native_bot = k
        .db()
        .store()
        .bots()
        .create(
            WS,
            &steve_store::NewBot {
                name: "Native".into(),
                harness: native_harness.to_string(),
                ..Default::default()
            },
            "me",
        )
        .unwrap()
        .unwrap();
    let item_native = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g1".into(),
            title: "t1".into(),
            bot: Some(native_bot.id),
            ..Default::default()
        })
        .unwrap();
    let s_native = k.db().sessions_of_item(&item_native.id).unwrap().remove(0);
    assert_eq!(
        k.rt.st()
            .system_contexts
            .get(&s_native.id)
            .map(String::as_str),
        Some("Me: run cargo check before every commit.")
    );
    let p_native = &k.rt.prompts_of(&s_native.id)[0];
    assert!(!p_native.contains("## User Context"), "{p_native}");
    assert!(
        !p_native.contains("Me: run cargo check before every commit."),
        "{p_native}"
    );
    assert!(
        p_native.contains("Task knowledge: SQLite WAL mode."),
        "{p_native}"
    );

    // Undeclared harness receives user_context under `## User Context` in opening protocol.
    let fallback_bot = k
        .db()
        .store()
        .bots()
        .create(
            WS,
            &steve_store::NewBot {
                name: "Fallback".into(),
                harness: fallback_harness.to_string(),
                ..Default::default()
            },
            "me",
        )
        .unwrap()
        .unwrap();
    let item_fallback = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g2".into(),
            title: "t2".into(),
            bot: Some(fallback_bot.id),
            ..Default::default()
        })
        .unwrap();
    let s_fallback = k
        .db()
        .sessions_of_item(&item_fallback.id)
        .unwrap()
        .remove(0);
    assert!(
        !k.rt.st().system_contexts.contains_key(&s_fallback.id),
        "fallback harness must not receive OpenSession.system_context"
    );
    let p_fallback = &k.rt.prompts_of(&s_fallback.id)[0];
    assert!(
        p_fallback.contains("## User Context\n\nMe: run cargo check before every commit."),
        "{p_fallback}"
    );
    assert!(
        p_fallback.contains("Task knowledge: SQLite WAL mode."),
        "{p_fallback}"
    );
}

fn router(k: &Kit, answer: &'static str) {
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router of Steve") {
            Ok(json!({"kind": "answer", "title": "问"}).to_string())
        } else if p.starts_with("You are Steve, the user's assistant.") {
            Ok(json!({"thought": "想", "answer": answer}).to_string())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

async fn reply(k: &Kit) -> crate::repo::Message {
    let mut ms = Vec::new();
    steve_runtime::testkit::eventually_sync(
        steve_runtime::testkit::on_timeout(|| async {
            format!("no reply: {:?}", k.db().messages(WS).unwrap())
        }),
        || {
            ms = k.db().messages(WS).unwrap();
            ms.len() >= 2 && !ms[1].meta["cites"].is_null()
        },
    )
    .await;
    ms[1].clone()
}

#[tokio::test]
async fn chat_context_goes_into_the_answer_prompt_and_chat_done_rewrites_the_reply() {
    let k = Kit::new();
    let seen = Log::default();
    let s2 = seen.clone();
    attach(&k, move |r| {
        r.prompt_context(Box::new(Ctx {
            session: "",
            chat: "",
            seen: s2.clone(),
        }))
        .prompt_context(Box::new(Ctx {
            session: "",
            chat: "[1] 笔记：Acme 用 Rust。",
            seen: s2,
        }));
    });
    router(&k, "用 Rust [1] [9]");
    k.core
        .say_as(WS, Some("u_1"), "聊聊 Acme", &[], &[])
        .unwrap();
    let m = reply(&k).await;
    assert_eq!(m.text, "用 Rust [1]");
    assert_eq!(m.meta["cites"], json!([{"n": 1}]));
    assert_eq!(m.meta["chat"], "聊聊 Acme");
    assert_eq!(m.meta["steps"]["answer"]["failed"], false);
    let calls = k.gw.calls.lock().unwrap().clone();
    let p = calls
        .iter()
        .rfind(|p| p.starts_with("You are Steve"))
        .unwrap();
    assert!(
        p.contains(&format!(
            "or null}}{}[1] 笔记：Acme 用 Rust。\n\nRecent conversation:\n",
            crate::gateway::STATIC_END
        )),
        "{p}"
    );
    /* both providers were asked; only the one that gave context sees the answer */
    assert_eq!(
        logged(&seen),
        [
            format!("chat {WS} u_1 聊聊 Acme"),
            format!("chat {WS} u_1 聊聊 Acme"),
            format!("done {} 用 Rust [1] [9] [1] 笔记：Acme 用 Rust。", m.id),
        ]
    );
}

#[tokio::test]
async fn chat_without_a_user_sees_as_the_workspace_and_without_context_is_unchanged() {
    let k = Kit::new();
    let seen = Log::default();
    let s2 = seen.clone();
    attach(&k, move |r| {
        r.prompt_context(Box::new(Ctx {
            session: "",
            chat: "",
            seen: s2,
        }));
    });
    router(&k, "你好呀");
    k.core.say(WS, "你好", &[]).unwrap();
    steve_runtime::testkit::eventually_sync("The workspace chat reply is stored.", || {
        k.db().messages(WS).unwrap().len() >= 2
    })
    .await;
    let ms = k.db().messages(WS).unwrap();
    assert_eq!(ms[1].text, "你好呀");
    assert!(ms[1].meta.get("cites").is_none());
    assert_eq!(logged(&seen), [format!("chat {WS}  你好")]);
    let calls = k.gw.calls.lock().unwrap().clone();
    let p = calls
        .iter()
        .rfind(|p| p.starts_with("You are Steve"))
        .unwrap();
    assert!(p.contains(&format!(
        "or null}}{}Recent conversation:\n",
        crate::gateway::STATIC_END
    )));
}

struct Undo {
    refuse: AtomicBool,
    log: Log,
}

#[test]
fn recovery_keeps_only_opted_in_cards_on_completed_items() {
    struct ResultCard;
    impl DecisionHandler for ResultCard {
        fn resolve(
            &self,
            _: &mut W<'_>,
            _: &DecisionCx,
            _: &Decision,
        ) -> ApiResult<DecisionOutcome> {
            Ok(json!({}).into())
        }

        fn retain_on_completed_item_recovery(&self, d: &Decision) -> bool {
            d.kind == "approve"
        }
    }

    let k = Kit::new();
    attach(&k, |r| {
        r.decision("result_card", Box::new(ResultCard));
    });
    let done = k.item("done", "完成", None);
    let dropped = k.item("drop", "作罢", None);
    let approval = k.decision(&done, None, "approve", "result_card", &["yes", "no"]);
    let ordinary = k.decision(&done, None, "approve", "other_card", &["yes", "no"]);
    let dropped = k.decision(&dropped, None, "approve", "result_card", &["yes", "no"]);
    k.core.recover().unwrap();

    let status = |id: &str| k.db().decision(id).unwrap().unwrap().status;
    assert_eq!(status(&approval), steve_types::DecisionStatus::Open);
    assert_eq!(status(&ordinary), steve_types::DecisionStatus::Expired);
    assert_eq!(status(&dropped), steve_types::DecisionStatus::Expired);
}

impl DecisionHandler for Undo {
    fn resolve(&self, _: &mut W<'_>, _: &DecisionCx, _: &Decision) -> ApiResult<DecisionOutcome> {
        Ok(json!({}).into())
    }
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        /* the handler sees the decision still resolved, inside a write transaction */
        let status = steve_store::DecisionRows(tx).get(&d.id)?.unwrap().status;
        self.log
            .lock()
            .unwrap()
            .push(format!("before_undo {} {status}", d.id));
        if self.refuse.load(Ordering::SeqCst) {
            return Err(ApiError::Conflict("已经发出去了，撤不回".into()));
        }
        Ok(())
    }
}

#[test]
fn undo_asks_the_module_of_the_decision_first_and_its_refusal_changes_nothing() {
    let k = Kit::new();
    let log = Log::default();
    let h = Arc::new(Undo {
        refuse: AtomicBool::new(true),
        log: log.clone(),
    });
    struct Shared(Arc<Undo>);
    impl DecisionHandler for Shared {
        fn resolve(
            &self,
            tx: &mut W<'_>,
            cx: &DecisionCx,
            d: &Decision,
        ) -> ApiResult<DecisionOutcome> {
            self.0.resolve(tx, cx, d)
        }
        fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
            self.0.before_undo(tx, d)
        }
    }
    let h2 = h.clone();
    attach(&k, move |r| {
        r.decision("mod_x", Box::new(Shared(h2)));
    });
    let id = k.item("run", "交付与验收", None);
    let s = k.session(&id, "idle");
    let d = k.decision(&id, Some(&s), "accept", "mod_x", &["pass", "rework"]);
    k.core
        .tx()
        .request_delivery_close(
            &id,
            "dev",
            crate::cycle::ClosePolicy::Human,
            false,
            "交付",
            Some(d.clone()),
            vec![],
        )
        .unwrap();
    // the delivering turn still runs, so the accepted result waits and its acceptance can be undone
    k.rt.st().turning.insert(s.clone());
    k.core.resolve(&d, "pass", "", "me").unwrap();
    assert_eq!(k.phase(&id), "closing");
    let e = k.core.undo_decision(&d, "me").unwrap_err();
    assert_eq!(e.to_string(), "已经发出去了，撤不回");
    assert_eq!(k.phase(&id), "closing");
    assert_eq!(
        k.db().decision(&d).unwrap().unwrap().status,
        steve_types::DecisionStatus::Resolved
    );
    assert!(!k.timeline_texts(&id).contains(&"撤销了决定".to_string()));
    h.refuse.store(false, Ordering::SeqCst);
    k.core.undo_decision(&d, "me").unwrap();
    assert!(!k.open_kinds(&id).is_empty());
    assert_eq!(
        k.db().decision(&d).unwrap().unwrap().status,
        steve_types::DecisionStatus::Open
    );
    k.rt.st().turning.remove(&s);
    assert_eq!(
        logged(&log),
        [
            format!("before_undo {d} resolved"),
            format!("before_undo {d} resolved")
        ]
    );
    /* a decision of another subject type is not the module's */
    let x = k.delivered(false);
    k.rt.st().turning.insert(x.session.clone());
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    k.core.undo_decision(&x.decision, "me").unwrap();
    assert_eq!(logged(&log).len(), 2);
}

/// A gateway that reports a cost, to see the B1 adapter pass it through.
struct Costly;
#[async_trait]
impl ModelGateway for Costly {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        _: Duration,
        tools: &crate::gateway::ThinkTools,
    ) -> crate::Result<Printed> {
        assert_eq!(*tools, crate::gateway::ThinkTools::None);
        Ok(Printed {
            text: format!("{tier:?}:{prompt}"),
            model: format!("m-{tier:?}"),
            cost_usd: Some(0.25),
            ..Default::default()
        })
    }
}

#[tokio::test]
async fn the_shared_gateway_maps_tiers_and_reports_model_and_cost() {
    let g = SharedGateway(Arc::new(Costly));
    let o = g
        .run(steve_modules::Tier::Fast, "hi", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!(
        (o.text.as_str(), o.model.as_str(), o.cost_usd),
        ("Fast:hi", "m-Fast", Some(0.25))
    );
    let s = g.smart("yo", Duration::from_secs(1)).await.unwrap();
    assert_eq!(s, "Smart:yo");
    /* the core's own gateway, shared: the fake's calls are recorded */
    let k = Kit::new();
    k.gw.set(|_, t| {
        if t == Tier::Smart {
            Ok("答".into())
        } else {
            Err(msg("no fast model"))
        }
    });
    let o = k
        .core
        .module_gateway()
        .run(steve_modules::Tier::Smart, "问", Duration::from_secs(1))
        .await
        .unwrap();
    assert_eq!((o.text.as_str(), o.model.as_str()), ("答", "fake-model"));
    assert_eq!(k.gw.calls.lock().unwrap().as_slice(), ["问"]);
    let e = k
        .core
        .module_gateway()
        .run(steve_modules::Tier::Fast, "问", Duration::from_secs(1))
        .await
        .unwrap_err();
    assert_eq!(e, "no fast model".into());
}

struct Boom {
    on: &'static str,
}

#[async_trait]
impl ItemHooks for Boom {
    fn name(&self) -> &'static str {
        "boom"
    }
    async fn on_finished(&self, _: &Item) {
        assert_ne!(self.on, "finished", "boom");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_observer_that_fails_changes_no_outcome_and_stops_no_other_hook() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Box::new(Boom { on: "finished" }))
            .item_hooks(Hook::new("b", &log));
    });
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let got = wait_log(&log, 1).await;
    assert_eq!(got, [format!("b finished {} closed", x.id)]);
    let i = k.db().item(&x.id).unwrap().unwrap();
    assert_eq!(
        (
            i.phase.as_str(),
            i.outcome.as_deref(),
            i.acceptance.as_deref()
        ),
        ("closed", Some("completed"), Some("human"))
    );
    // the worker lives on: the next item's hooks still run
    let y = k.delivered(false);
    k.core.resolve(&y.decision, "pass", "", "me").unwrap();
    assert_eq!(wait_log(&log, 2).await.len(), 2);
    assert_eq!(k.phase(&y.id), "closed");
}

//! The B1 call points with registered fakes: item hooks at create / accept / finish / drop (first
//! `Takeover` wins), prompt context in the session protocol and in chat answers, `before_undo` of a
//! module decision, and steve-core's gateway shared as the modules' `Gateway`.

use crate::error::msg;
use crate::gateway::{ClaudeCli, ModelGateway, Printed, SharedGateway};
use crate::repo::{Item, WS};
use crate::runtime::Check;
use crate::testkit::Kit;
use crate::{CreateItem, ItemAction, Tier};
use async_trait::async_trait;
use rusqlite::Connection;
use serde_json::json;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use steve_modules::context::{ChatCtx, ChatDone, PromptContext};
use steve_modules::decisions::{DecisionCx, DecisionHandler};
use steve_modules::hooks::{ItemHooks, Takeover};
use steve_modules::{ApiError, ApiResult, Gateway, Modules, Registry};
use steve_store::base::Viewer;
use steve_store::{Decision, Tx};

type Log = Arc<Mutex<Vec<String>>>;

fn attach(k: &Kit, f: impl FnOnce(&mut Registry)) {
    let mut r = Registry::new();
    f(&mut r);
    let m = Modules::new(k.db().store().clone(), k.core.module_gateway(), r);
    k.core.attach_modules(m);
}

struct Hook {
    name: &'static str,
    take: bool,
    log: Log,
}

impl Hook {
    fn new(name: &'static str, take: bool, log: &Log) -> Box<Self> {
        Box::new(Self {
            name,
            take,
            log: log.clone(),
        })
    }
    fn note(&self, what: &str, i: &Item) {
        self.log
            .lock()
            .unwrap()
            .push(format!("{} {what} {} {}", self.name, i.id, i.status));
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
    async fn on_accepted(&self, i: &Item) -> Option<Takeover> {
        self.note("accepted", i);
        self.take.then_some(Takeover { by: self.name })
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

/// Waits (≤3 s) until the log has `n` entries.
async fn until(log: &Log, n: usize) -> Vec<String> {
    for _ in 0..300 {
        if log.lock().unwrap().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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
        r.item_hooks(Hook::new("a", false, &log))
            .item_hooks(Hook::new("b", false, &log));
    });
    let i = new_item(&k, "写一首诗");
    assert_eq!(
        until(&log, 2).await,
        ["a created ST-1 run", "b created ST-1 run"]
    );
    k.core.item_action(&i.id, ItemAction::Drop, "me").unwrap();
    assert_eq!(
        until(&log, 4).await[2..],
        ["a dropped ST-1 drop", "b dropped ST-1 drop"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_proposal_declined_is_a_drop() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", false, &log));
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
        until(&log, 2).await,
        ["a created ST-1 wait", "a dropped ST-1 drop"]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn accepted_asks_every_hook_then_core_finishes_and_reports_it() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", false, &log))
            .item_hooks(Hook::new("b", false, &log));
    });
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    let got = until(&log, 4).await;
    assert_eq!(
        got,
        [
            format!("a accepted {} run", x.id),
            format!("b accepted {} run", x.id),
            format!("a finished {} done", x.id),
            format!("b finished {} done", x.id),
        ]
    );
    let (status, stage, now) = k.status(&x.id);
    assert_eq!(
        (status.as_str(), stage.as_str(), now.as_str()),
        ("done", "收尾", "已验收")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_first_takeover_wins_and_core_does_not_finish() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", false, &log))
            .item_hooks(Hook::new("gh", true, &log))
            .item_hooks(Hook::new("c", true, &log));
    });
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    until(&log, 2).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        logged(&log),
        [
            format!("a accepted {} run", x.id),
            format!("gh accepted {} run", x.id)
        ]
    );
    assert_eq!(k.status(&x.id).0, "run");
    assert!(k.timeline_texts(&x.id).contains(&"验收通过".to_string()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_acceptance_undone_before_the_hooks_answered_does_not_finish() {
    struct Slow(Log);
    #[async_trait]
    impl ItemHooks for Slow {
        fn name(&self) -> &'static str {
            "slow"
        }
        async fn on_accepted(&self, _: &Item) -> Option<Takeover> {
            tokio::time::sleep(Duration::from_millis(200)).await;
            self.0.lock().unwrap().push("asked".into());
            None
        }
    }
    let k = Kit::new();
    let log = Log::default();
    let l = log.clone();
    attach(&k, |r| {
        r.item_hooks(Box::new(Slow(l)));
    });
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    k.core.undo_decision(&x.decision, "me").unwrap();
    until(&log, 1).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    /* back to waiting for the reopened acceptance */
    assert_eq!(k.status(&x.id).0, "wait");
    assert_eq!(
        k.db().decision(&x.decision).unwrap().unwrap().status,
        "open"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_acceptance_asks_the_hooks_too() {
    let k = Kit::new();
    let log = Log::default();
    attach(&k, |r| {
        r.item_hooks(Hook::new("a", false, &log));
    });
    let x = k.delivered(false);
    k.db()
        .decision_resolve(&x.decision, json!({"choice": "skip"}), "resolved", None)
        .unwrap();
    k.db()
        .settings_set(
            WS,
            &json!({"accDefault": "auto"}).as_object().unwrap().clone(),
            "me",
        )
        .unwrap();
    let s = k.db().session(&x.session).unwrap().unwrap();
    k.core
        .tool_deliver(
            &s,
            "ok",
            &[],
            &[Check {
                name: "tests".into(),
                passed: true,
            }],
            &[],
        )
        .unwrap();
    assert_eq!(
        until(&log, 2).await,
        [
            format!("a accepted {} run", x.id),
            format!("a finished {} done", x.id)
        ]
    );
    assert_eq!(k.status(&x.id).2, "已自动验收");
}

#[test]
fn without_hooks_acceptance_finishes_synchronously_even_with_modules_attached() {
    let k = Kit::new();
    attach(&k, |_| {});
    let x = k.delivered(false);
    k.core.resolve(&x.decision, "pass", "", "me").unwrap();
    assert_eq!(k.status(&x.id).0, "done");
}

struct Ctx {
    session: &'static str,
    chat: &'static str,
    seen: Log,
}

impl PromptContext for Ctx {
    fn session(&self, _: &Connection, ws: &str, item: &Item) -> Option<String> {
        self.seen
            .lock()
            .unwrap()
            .push(format!("session {ws} {}", item.id));
        Some(self.session.into())
    }
    fn chat(&self, _: &Connection, ws: &str, v: &Viewer, text: &str) -> Option<ChatCtx> {
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
        _: &Connection,
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

fn router(k: &Kit, answer: &'static str) {
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router of Steve") {
            Ok(json!({"kind": "chat", "title": "问"}).to_string())
        } else if p.starts_with("You are Steve, the user's assistant.") {
            Ok(json!({"thought": "想", "answer": answer}).to_string())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

async fn reply(k: &Kit) -> crate::repo::Message {
    for _ in 0..300 {
        let ms = k.db().messages(WS).unwrap();
        if ms.len() >= 2 && !ms[1].meta["cites"].is_null() {
            return ms[1].clone();
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("no reply: {:?}", k.db().messages(WS).unwrap());
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
        .say_as(WS, Some("u_1"), "Acme 用什么？", &[], &[])
        .unwrap();
    let m = reply(&k).await;
    assert_eq!(m.text, "用 Rust [1]");
    assert_eq!(m.meta["cites"], json!([{"n": 1}]));
    assert_eq!(m.meta["chat"], "Acme 用什么？");
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
            format!("chat {WS} u_1 Acme 用什么？"),
            format!("chat {WS} u_1 Acme 用什么？"),
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
    for _ in 0..300 {
        if k.db().messages(WS).unwrap().len() >= 2 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
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

impl DecisionHandler for Undo {
    fn resolve(&self, _: &Tx, _: &DecisionCx, _: &Decision) -> ApiResult<serde_json::Value> {
        Ok(json!({}))
    }
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        /* the handler sees the decision still resolved, inside a write transaction */
        let status: String =
            tx.query_row("SELECT status FROM decisions WHERE id = ?", [&d.id], |r| {
                r.get(0)
            })?;
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
        fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<serde_json::Value> {
            self.0.resolve(tx, cx, d)
        }
        fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
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
    k.core.resolve(&d, "pass", "", "me").unwrap();
    assert_eq!(k.status(&id).0, "done");
    let e = k.core.undo_decision(&d, "me").unwrap_err();
    assert_eq!(e.to_string(), "已经发出去了，撤不回");
    assert_eq!(k.status(&id).0, "done");
    assert_eq!(k.db().decision(&d).unwrap().unwrap().status, "resolved");
    assert!(!k.timeline_texts(&id).contains(&"撤销了决定".to_string()));
    h.refuse.store(false, Ordering::SeqCst);
    k.core.undo_decision(&d, "me").unwrap();
    assert_eq!(k.status(&id).0, "wait");
    assert_eq!(k.db().decision(&d).unwrap().unwrap().status, "open");
    assert_eq!(
        logged(&log),
        [
            format!("before_undo {d} resolved"),
            format!("before_undo {d} resolved")
        ]
    );
    /* a decision of another subject type is not the module's */
    let x = k.delivered(false);
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
        extra: &[String],
    ) -> crate::Result<Printed> {
        assert!(extra.is_empty());
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
    assert_eq!(e, "no fast model");
}

#[cfg(unix)]
#[tokio::test]
async fn claude_cli_falls_back_down_the_tier_and_reports_total_cost() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("claude");
    /* args: -p <prompt> --output-format json --model <m> */
    std::fs::write(
        &exe,
        "#!/bin/sh\nif [ \"$6\" = bad ]; then echo nope >&2; exit 1; fi\nprintf '{\"result\":\"ok %s\",\"total_cost_usd\":0.0123}' \"$6\"\n",
    )
    .unwrap();
    std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
    let cli: Arc<dyn ModelGateway> = Arc::new(ClaudeCli {
        codex: None,
        executable: exe,
        fast: vec!["bad".into(), "good".into()],
        smart: vec![],
        cwd: dir.path().into(),
        models: None,
    });
    let o = SharedGateway(cli.clone())
        .run(steve_modules::Tier::Fast, "hi", Duration::from_secs(10))
        .await
        .unwrap();
    assert_eq!(
        (o.text.as_str(), o.model.as_str(), o.cost_usd),
        ("ok good", "good", Some(0.0123))
    );
    let e = SharedGateway(cli)
        .run(steve_modules::Tier::Smart, "hi", Duration::from_secs(10))
        .await
        .unwrap_err();
    assert_eq!(e, "没有可用的模型");
}

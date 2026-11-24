//! W3.4 synthetic replay fixtures — Core driver (104 scenarios).
//!
//! Exercises the real `steve-core` thread pipeline against SQLite (`Kit`), `FakeGateway`,
//! `SingleModelGateway`, `FakeResearch`, `FakeRuntime`, and `reopened()` restarts.
//!
//! Run in baseline mode (pins current `main` discrepancies):
//!   `cargo test --manifest-path hub-rs/Cargo.toml -p steve-core --lib w34_thread_replay_core -- --nocapture`
//! Run in strict W3.4 acceptance mode (fails on any invariant violation):
//!   `STEVE_THREAD_REPLAY_STRICT=1 cargo test --manifest-path hub-rs/Cargo.toml -p steve-core --lib w34_thread_replay_core -- --nocapture`

#[path = "../../../../tests/thread_replay/report.rs"]
mod report;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_store::attention;
use steve_store::{NewThreadRun, NewWorkspace, RunPatch, core_queue, thread_runs};
use tokio::sync::{Barrier, Notify};

use crate::error::{CoreError, Result, msg};
use crate::gateway::{
    CallSpec, GatewayError, ModelGateway, OneCall, OneModel, OneShotError, Readable,
    SingleModelGateway, ThinkTools, Tier, TimeoutPhase, Tried,
};
use crate::repo::{Message, WS};
use crate::testkit::{FakeResearch, Kit, Step};
use crate::thread::{
    A2uiAction, Conv, ConvHost, MAIN, RerouteTo, ResearchOutcome, ResearchRequest, ResearchSink,
    Researcher, RunEnd, SayTo,
};
use report::Report;

fn replay_kit(no_resume: bool) -> Kit {
    Kit::with(|cfg| {
        cfg.no_resume = no_resume;
        cfg.port = 18799;
    })
}

fn second_ws(k: &Kit) -> String {
    let owner = k
        .db()
        .store()
        .users()
        .create("local:owner_ws2", "Owner2", None, "owner")
        .unwrap();
    let ws2 = k
        .db()
        .store()
        .workspaces()
        .create(
            &NewWorkspace {
                name: "Second".into(),
                description: None,
                item_prefix: Some("W2".into()),
            },
            &owner,
        )
        .unwrap();
    k.db()
        .bot_seed("dev", false, crate::orchestrator::dev_bot(None), &ws2.id)
        .unwrap();
    ws2.id
}

fn router_calls(k: &Kit) -> usize {
    k.gw.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with("You are the router"))
        .count()
}

fn answer_calls(k: &Kit) -> usize {
    k.gw.calls
        .lock()
        .unwrap()
        .iter()
        .filter(|p| p.starts_with("You are Steve"))
        .count()
}

fn set_scripted_gateway(k: &Kit, route_json: Value, answer_text: &'static str) {
    let r = route_json.to_string();
    k.gw.set(move |p, _| {
        if p.starts_with("You are the router") {
            Ok(r.clone())
        } else if p.starts_with("You are Steve") {
            Ok(json!({"thought": "", "answer": answer_text, "offer": null}).to_string())
        } else {
            Err(msg("unexpected prompt"))
        }
    });
}

fn steve_replies_for(k: &Kit, ws: &str, space: &str, origin: &str) -> Vec<Message> {
    k.db()
        .messages_in(ws, space, 200)
        .unwrap()
        .into_iter()
        .filter(|m| m.author == "steve" && m.meta["origin"].as_str() == Some(origin))
        .collect()
}

fn steve_messages(k: &Kit, ws: &str, space: &str) -> Vec<Message> {
    k.db()
        .messages_in(ws, space, 200)
        .unwrap()
        .into_iter()
        .filter(|m| m.author == "steve")
        .collect()
}

async fn wait_until<F: FnMut() -> bool>(label: &str, mut cond: F) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if cond() {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timeout waiting for: {label}"
        );
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
}

async fn settle_space(k: &Kit, ws: &str, space: &str, min_msgs: usize) -> Vec<Message> {
    let mut out = vec![];
    wait_until(&format!("{ws}/{space} >= {min_msgs}"), || {
        out = k.db().messages_in(ws, space, 200).unwrap();
        out.len() >= min_msgs
    })
    .await;
    out
}

async fn settle_replies(k: &Kit, ws: &str, space: &str, origin: &str, min: usize) -> Vec<Message> {
    let mut out = vec![];
    wait_until(&format!("replies for {origin} >= {min}"), || {
        out = steve_replies_for(k, ws, space, origin);
        out.len() >= min
    })
    .await;
    out
}

async fn wait_idle_main(k: &Kit, ws: &str) {
    wait_until(&format!("main idle {ws}"), || {
        !k.core.main_busy(ws).unwrap()
    })
    .await;
}

/// Custom ConvHost for guest conversation replay scenarios.
#[derive(Default)]
struct ReplayHost {
    answered: Mutex<Vec<String>>,
    research_prepared: Mutex<Vec<String>>,
    said: Mutex<Vec<(String, String)>>,
    answer_err: Mutex<Option<GatewayError>>,
    block_answer: Mutex<Option<(Arc<Notify>, Arc<Notify>)>>,
    research_context_text: Mutex<String>,
}

impl ReplayHost {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            research_context_text: Mutex::new("GUEST_SCOPE_CONTEXT_OPS".into()),
            ..Default::default()
        })
    }
}

#[async_trait]
impl ConvHost for ReplayHost {
    async fn answer(
        &self,
        _ws: &str,
        _space: &str,
        origin: &str,
    ) -> std::result::Result<String, GatewayError> {
        self.answered.lock().unwrap().push(origin.into());
        let gate = self.block_answer.lock().unwrap().clone();
        if let Some((entered, release)) = gate {
            entered.notify_one();
            release.notified().await;
        }
        if let Some(e) = self.answer_err.lock().unwrap().clone() {
            return Err(e);
        }
        Ok("渠道助手的回答".into())
    }

    fn research_context(&self, _ws: &str, _space: &str, _origin: &str) -> String {
        self.research_context_text.lock().unwrap().clone()
    }

    async fn research_answer(
        &self,
        _ws: &str,
        _space: &str,
        origin: &str,
        text: &str,
    ) -> std::result::Result<String, String> {
        self.research_prepared.lock().unwrap().push(origin.into());
        Ok(format!("已核验：{text}"))
    }

    fn said(&self, _ws: &str, space: &str, id: &str) {
        self.said.lock().unwrap().push((space.into(), id.into()));
    }
}

/// Custom Researcher that ignores `stop` or reports configurable end/offer/concurrency.
struct ScriptedResearcher {
    entered: Arc<Notify>,
    release: Arc<Notify>,
    released: std::sync::atomic::AtomicBool,
    seen: Mutex<Vec<ResearchRequest>>,
    ignore_stop: bool,
    outcome_end: RunEnd,
    outcome_text: &'static str,
    outcome_offer: Option<crate::gateway::Offer>,
    outcome_error: Option<&'static str>,
    in_flight: AtomicUsize,
    max_in_flight: AtomicUsize,
}

impl ScriptedResearcher {
    fn new(end: RunEnd, text: &'static str) -> Arc<Self> {
        Arc::new(Self {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: false,
            outcome_end: end,
            outcome_text: text,
            outcome_offer: None,
            outcome_error: None,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        })
    }

    async fn wait_entered(&self) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
        while self.seen.lock().unwrap().is_empty() {
            let fut = self.entered.notified();
            tokio::pin!(fut);
            fut.as_mut().enable();
            if !self.seen.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::timeout_at(deadline, fut)
                .await
                .expect("timeout waiting for ScriptedResearcher to enter");
        }
    }

    fn open(&self) {
        self.released.store(true, Ordering::SeqCst);
        self.release.notify_waiters();
    }
}

#[async_trait]
impl Researcher for ScriptedResearcher {
    async fn research(&self, req: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
        self.max_in_flight.fetch_max(cur, Ordering::SeqCst);
        self.seen.lock().unwrap().push(req.clone());
        if !self.outcome_text.is_empty() {
            sink.text(self.outcome_text);
        }
        self.entered.notify_waiters();
        let wait_release = async {
            while !self.released.load(Ordering::SeqCst) {
                let fut = self.release.notified();
                tokio::pin!(fut);
                fut.as_mut().enable();
                if self.released.load(Ordering::SeqCst) {
                    break;
                }
                fut.await;
            }
        };
        if self.ignore_stop {
            wait_release.await;
        } else {
            tokio::select! {
                _ = wait_release => {}
                _ = async {
                    while !req.stop.load(Ordering::SeqCst) {
                        tokio::time::sleep(Duration::from_millis(2)).await;
                    }
                } => {}
            }
        }
        self.in_flight.fetch_sub(1, Ordering::SeqCst);
        let end = if !self.ignore_stop && req.stop.load(Ordering::SeqCst) {
            RunEnd::Stopped
        } else {
            self.outcome_end
        };
        ResearchOutcome {
            text: self.outcome_text.into(),
            offer: self.outcome_offer.clone(),
            end,
            model: Some("scripted-research".into()),
            session: Some("sess_1".into()),
            turns: 1,
            error: self.outcome_error.map(String::from),
            ..Default::default()
        }
    }
}

/// Custom OneModel for W3.1/W3.4 gateway preflight & timeout tests.
struct ScriptedOneModel {
    calls: AtomicUsize,
    allow_tools: bool,
    fail_with: Mutex<Option<OneShotError>>,
}

#[async_trait]
impl OneModel for ScriptedOneModel {
    async fn preflight(&self, call: &OneCall<'_>) -> Result<()> {
        if call.spec.required_tools.any() && !self.allow_tools {
            return Err(GatewayError::MissingCapability.into());
        }
        Ok(())
    }

    async fn once(
        &self,
        _call: &OneCall<'_>,
        _model: &str,
        _effort: Option<&str>,
        _on_text: Option<crate::gateway::OnText<'_>>,
    ) -> Tried {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(err) = self.fail_with.lock().unwrap().take() {
            return Tried::of(Err(err), vec![]);
        }
        Tried::of(Ok(("ok".into(), None)), vec![])
    }
}

/// Custom ModelGateway that emits partial streaming tokens and then errors.
struct FailingStreamGateway {
    calls: AtomicUsize,
}

#[async_trait]
impl ModelGateway for FailingStreamGateway {
    async fn call(
        &self,
        _prompt: &str,
        _tier: Tier,
        _t: Duration,
        _tools: &ThinkTools,
    ) -> Result<crate::gateway::Printed> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Err(GatewayError::Timeout(TimeoutPhase::Execution).into())
    }

    async fn call_streaming(
        &self,
        _prompt: &str,
        _tier: Tier,
        _t: Duration,
        _tools: &ThinkTools,
        on_text: crate::gateway::OnText<'_>,
    ) -> Result<crate::gateway::Printed> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        on_text(r#"{"thought":"","answer":"半截流式输出"}"#);
        Err(GatewayError::Timeout(TimeoutPhase::Execution).into())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn w34_thread_replay_core() {
    let mut report = Report::default();
    run_submit(&mut report).await;
    run_queued(&mut report).await;
    run_claim(&mut report).await;
    run_acl(&mut report).await;
    run_cancel(&mut report).await;
    run_stream(&mut report).await;
    run_restart(&mut report).await;
    run_spaces(&mut report).await;
    run_routing(&mut report).await;
    report.finish(104);
}

// =========================================================================
// 1. Submit / Idempotency (10 cases)
// =========================================================================

async fn run_submit(report: &mut Report) {
    // 01: Duplicate logical submit via Core::say (no durable client_msg_id in Core)
    {
        let k = replay_kit(true);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "收到");
        let m1 = k.core.say(WS, "你好呀", &[]).unwrap();
        let m2 = k.core.say(WS, "你好呀", &[]).unwrap();
        settle_space(&k, WS, MAIN, 4).await;
        wait_idle_main(&k, WS).await;
        let replies = steve_messages(&k, WS, MAIN).len();
        report.case(
            "core.submit.01_duplicate_core_say_same_text",
            &[
                ("submit_idempotent_key_deduped", m1 == m2),
                ("le1_final_reply_per_intent", replies <= 1),
            ],
            &[
                "submit_idempotent_key_deduped",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 02: Duplicate Core::say for an item request creates 2 items without submit key
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "修按钮", "goal": "改颜色"}),
            "ok",
        );
        let m1 = k.core.say(WS, "帮我把登录按钮改成蓝色", &[]).unwrap();
        let m2 = k.core.say(WS, "帮我把登录按钮改成蓝色", &[]).unwrap();
        settle_replies(&k, WS, MAIN, &m1, 1).await;
        settle_replies(&k, WS, MAIN, &m2, 1).await;
        wait_idle_main(&k, WS).await;
        let items = k.db().items(WS).unwrap().len();
        report.case(
            "core.submit.02_duplicate_core_say_item_dispatch",
            &[("le1_dispatch_per_intent", items <= 1)],
            &["le1_dispatch_per_intent"],
        );
    }

    // 03: Whitespace-only message must be rejected at rule/entry layer with 0 router calls
    {
        let k = replay_kit(true);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let res = k.core.say(WS, "   ", &[]);
        if let Ok(mid) = &res {
            settle_replies(&k, WS, MAIN, mid, 1).await;
            wait_idle_main(&k, WS).await;
        }
        report.case(
            "core.submit.03_empty_text_rejected_no_router",
            &[
                ("empty_message_rejected", res.is_err()),
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
            ],
            &["empty_message_rejected", "explicit_route_zero_router_calls"],
        );
    }

    // 04: Newline/tab-only message must make 0 router calls
    {
        let k = replay_kit(true);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let res = k.core.say(WS, "\n\t  \n", &[]);
        if let Ok(mid) = &res {
            settle_replies(&k, WS, MAIN, mid, 1).await;
            wait_idle_main(&k, WS).await;
        }
        report.case(
            "core.submit.04_whitespace_only_newlines_no_router",
            &[
                ("empty_message_rejected", res.is_err()),
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
            ],
            &["empty_message_rejected", "explicit_route_zero_router_calls"],
        );
    }

    // 05: Message + pending task durability gap (say_with writes message and core_queue in 2 txs)
    {
        let k = replay_kit(false);
        // Simulate crash between message_add and core_queue::enqueue (non-atomic 2-tx submit)
        let orphan_mid = k
            .db()
            .message_add(WS, "me", "断电测试消息", None, None)
            .unwrap();
        let k2 = k.reopened();
        k2.thread_recover().unwrap();
        let ready = k2.thread_ready().unwrap();
        let queued_pending = k
            .db()
            .store()
            .read(|r| core_queue::pending(r, WS, &orphan_mid))
            .unwrap()
            .is_some();
        let replies = steve_replies_for(&k, WS, MAIN, &orphan_mid);
        // In current 2-tx design, the message is unqueued (lost from queue) and gets a rerun notice
        // instead of being impossible due to single-tx SubmitMessage.
        let atomic_submit_guarantee = queued_pending && ready.resumed == 0 && replies.is_empty();
        report.case(
            "core.submit.05_say_atomic_message_and_task",
            &[
                ("unconfirmed_start_zero_auto_replay", router_calls(&k) == 0),
                ("submit_atomic_message_and_task", atomic_submit_guarantee),
            ],
            &["submit_atomic_message_and_task"],
        );
    }

    // 06: Cross-workspace explicit item target must be rejected fail-closed with 0 foreign writes
    {
        let k = replay_kit(true);
        let ws2 = second_ws(&k);
        let foreign_item = k
            .db()
            .item_create(
                &ws2,
                &crate::repo::NewItem {
                    tags: k.task_tags(&ws2),
                    title: "异空间事项".into(),
                    goal: "秘密目标".into(),
                    bot_id: "dev".into(),
                    status: Some("run".into()),
                    stage: Some("执行".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .id;
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let res = k.core.say_to(
            WS,
            None,
            "加一条补充",
            &[],
            &[],
            SayTo {
                item: Some(foreign_item.clone()),
                reply_to: None,
            },
        );
        if let Ok(mid) = &res {
            settle_replies(&k, WS, MAIN, mid, 1).await;
            wait_idle_main(&k, WS).await;
        }
        let foreign_tl = k.db().timeline(&foreign_item).unwrap();
        report.case(
            "core.submit.06_cross_ws_item_target_rejected",
            &[
                ("cross_scope_ref_no_foreign_write", foreign_tl.is_empty()),
                ("cross_scope_ref_rejected", res.is_err()),
            ],
            &["cross_scope_ref_rejected"],
        );
    }

    // 07: Cross-workspace reply_to must be rejected fail-closed at entry
    {
        let k = replay_kit(true);
        let ws2 = second_ws(&k);
        let foreign_msg = k
            .db()
            .message_add(&ws2, "steve", "外部工作区消息", None, None)
            .unwrap();
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let res = k.core.say_to(
            WS,
            None,
            "回复这条",
            &[],
            &[],
            SayTo {
                item: None,
                reply_to: Some(foreign_msg),
            },
        );
        if let Ok(mid) = &res {
            settle_replies(&k, WS, MAIN, mid, 1).await;
            wait_idle_main(&k, WS).await;
        }
        report.case(
            "core.submit.07_cross_ws_reply_to_rejected",
            &[("cross_scope_ref_rejected", res.is_err())],
            &["cross_scope_ref_rejected"],
        );
    }

    // 08: Cross-space reply_to (MAIN quoting guest space message) must be rejected fail-closed
    {
        let k = replay_kit(true);
        let guest_msg = k
            .db()
            .message_add_in(
                WS,
                "chan:sp_secret",
                "steve",
                "访客空间内容",
                None,
                &json!({}),
            )
            .unwrap();
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let res = k.core.say_to(
            WS,
            None,
            "回复访客消息",
            &[],
            &[],
            SayTo {
                item: None,
                reply_to: Some(guest_msg),
            },
        );
        if let Ok(mid) = &res {
            settle_replies(&k, WS, MAIN, mid, 1).await;
            wait_idle_main(&k, WS).await;
        }
        report.case(
            "core.submit.08_cross_space_reply_to_rejected",
            &[("cross_scope_ref_rejected", res.is_err())],
            &["cross_scope_ref_rejected"],
        );
    }

    // 09: Duplicate guest say_in with identical provider origin creates 2 messages & 2 replies
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let conv = Conv {
            space: "chan:sp_dup".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let meta = json!({"chan": {"message": "cm_dup_1", "space": "sp_dup"}});
        let m1 = k.core.say_in(WS, conv.clone(), "查一下", &meta).unwrap();
        let m2 = k.core.say_in(WS, conv, "查一下", &meta).unwrap();
        settle_replies(&k, WS, "chan:sp_dup", &m1, 1).await;
        settle_replies(&k, WS, "chan:sp_dup", &m2, 1).await;
        let total_replies = steve_messages(&k, WS, "chan:sp_dup").len();
        report.case(
            "core.submit.09_duplicate_say_in_guest_turn",
            &[
                ("submit_idempotent_key_deduped", m1 == m2),
                ("le1_final_reply_per_intent", total_replies <= 1),
            ],
            &[
                "submit_idempotent_key_deduped",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 10: A2UI double submit on same (block, surface) is idempotent
    {
        let k = replay_kit(true);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "已处理");
        let reply_id = k
            .db()
            .message_add(
                WS,
                "steve",
                "请确认：\n```a2ui\n{\"surfaceId\":\"surf1\"}\n```",
                None,
                None,
            )
            .unwrap();
        let act = A2uiAction {
            block: "blk1".into(),
            surface: "surf1".into(),
            component: "btn_ok".into(),
            name: "confirm".into(),
            label: "确认".into(),
            context: json!({"v": 1}),
            data: json!({"v": 1}),
            timestamp: "2026-11-24T00:00:00Z".into(),
        };
        let a1 = k
            .core
            .a2ui_action(WS, None, &reply_id, act.clone())
            .unwrap();
        let a2 = k.core.a2ui_action(WS, None, &reply_id, act).unwrap();
        let user_mid = a1.message_id.clone().unwrap();
        settle_replies(&k, WS, MAIN, &user_mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.submit.10_a2ui_double_submit_idempotent",
            &[
                ("first_submit_accepted", !a1.already),
                (
                    "second_submit_already",
                    a2.already && a2.message_id == a1.message_id,
                ),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &user_mid).len() == 1,
                ),
            ],
            &[],
        );
    }
}

// =========================================================================
// 2. Queued Edit / Withdraw (12 cases)
// =========================================================================

async fn hold_main_turn(k: &Kit) -> (Arc<FakeResearch>, String) {
    let fr = FakeResearch::new(vec![Step::Text("第一条结果"), Step::Gate]);
    k.core.set_researcher(fr.clone());
    let first = k.core.say(WS, "第一条为什么慢？", &[]).unwrap();
    wait_until("first research running", || {
        !fr.seen.lock().unwrap().is_empty()
    })
    .await;
    (fr, first)
}

async fn run_queued(report: &mut Report) {
    // 01: Edit queued message before claim routes new text
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let q = k.core.say(WS, "旧问题为什么？", &[]).unwrap();
        let edited = k.core.thread_queued_edit(WS, &q, "新问题为什么？").unwrap();
        fr.open(2);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &q, 1).await;
        wait_idle_main(&k, WS).await;
        let seen = fr.seen.lock().unwrap();
        let got_new = seen
            .iter()
            .any(|r| r.mid == q && r.question == "新问题为什么？");
        report.case(
            "core.queued.01_edit_before_claim_routes_new_text",
            &[
                ("edit_accepted", edited),
                ("routed_with_edited_text", got_new),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &q).len() == 1,
                ),
            ],
            &[],
        );
    }

    // 02: Edit queued message twice keeps latest text and original order
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let q1 = k.core.say(WS, "排队一为什么？", &[]).unwrap();
        let q2 = k.core.say(WS, "排队二为什么？", &[]).unwrap();
        let e1 = k
            .core
            .thread_queued_edit(WS, &q1, "排队一改1为什么？")
            .unwrap();
        let e2 = k
            .core
            .thread_queued_edit(WS, &q1, "排队一改2为什么？")
            .unwrap();
        fr.open(3);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &q1, 1).await;
        settle_replies(&k, WS, MAIN, &q2, 1).await;
        wait_idle_main(&k, WS).await;
        let seen = fr.seen.lock().unwrap();
        let pos1 = seen.iter().position(|r| r.mid == q1);
        let pos2 = seen.iter().position(|r| r.mid == q2);
        let latest = seen
            .iter()
            .find(|r| r.mid == q1)
            .is_some_and(|r| r.question == "排队一改2为什么？");
        report.case(
            "core.queued.02_edit_twice_keeps_latest_and_order",
            &[
                ("both_edits_accepted", e1 && e2),
                ("latest_text_used", latest),
                ("order_preserved", pos1 < pos2),
            ],
            &[],
        );
    }

    // 03: Withdraw queued message before claim yields 0 replies & returns text/files
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let f = k.file("note.txt", "text/plain", b"abc");
        let q = k
            .core
            .say(WS, "要撤回的问题为什么？", std::slice::from_ref(&f))
            .unwrap();
        let wd = k.core.thread_queued_withdraw(WS, &q).unwrap();
        fr.open(2);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        let returned_ok = wd
            .as_ref()
            .is_some_and(|(t, fs)| t == "要撤回的问题为什么？" && fs == std::slice::from_ref(&f));
        let zero_replies = steve_replies_for(&k, WS, MAIN, &q).is_empty();
        let zero_exec = !fr.seen.lock().unwrap().iter().any(|r| r.mid == q);
        report.case(
            "core.queued.03_withdraw_before_claim_zero_reply",
            &[
                ("withdraw_returned_payload", returned_ok),
                ("withdrawn_zero_replies", zero_replies),
                ("withdrawn_zero_execution", zero_exec),
            ],
            &[],
        );
    }

    // 04: Double withdraw is idempotent
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let q = k.core.say(WS, "双撤回为什么？", &[]).unwrap();
        let w1 = k.core.thread_queued_withdraw(WS, &q).unwrap();
        let w2 = k.core.thread_queued_withdraw(WS, &q).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.queued.04_double_withdraw_idempotent",
            &[
                ("first_withdraw_some", w1.is_some()),
                ("second_withdraw_none", w2.is_none()),
            ],
            &[],
        );
    }

    // 05: Edit after withdraw rejected
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let q = k.core.say(WS, "撤后改为什么？", &[]).unwrap();
        let w1 = k.core.thread_queued_withdraw(WS, &q).unwrap().is_some();
        let edit_after = k.core.thread_queued_edit(WS, &q, "改不动为什么？").unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.queued.05_edit_after_withdraw_rejected",
            &[("withdraw_ok", w1), ("edit_rejected", !edit_after)],
            &[],
        );
    }

    // 06: Edit after claim rejected; frozen input preserved
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let edited = k.core.thread_queued_edit(WS, &first, "改运行中的").unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        let orig = k.db().message(&first).unwrap().unwrap().text;
        report.case(
            "core.queued.06_edit_after_claim_rejected",
            &[
                ("edit_rejected", !edited),
                ("frozen_text_preserved", orig == "第一条为什么慢？"),
            ],
            &[],
        );
    }

    // 07: Withdraw after claim rejected; reply finishes normally
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let wd = k.core.thread_queued_withdraw(WS, &first).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.queued.07_withdraw_after_claim_rejected",
            &[
                ("withdraw_rejected", wd.is_none()),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &first).len() == 1,
                ),
            ],
            &[],
        );
    }

    // 08: Edit in guest space rejected
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *host.block_answer.lock().unwrap() = Some((entered.clone(), release.clone()));
        k.core.set_conv_host(host.clone());
        let conv = Conv {
            space: "chan:sp_edit".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let m1 = k
            .core
            .say_in(WS, conv.clone(), "第一条为什么？", &json!({}))
            .unwrap();
        entered.notified().await;
        let m2 = k
            .core
            .say_in(WS, conv, "第二条为什么？", &json!({}))
            .unwrap();
        let edited = k.core.thread_queued_edit(WS, &m2, "改访客").unwrap();
        *host.block_answer.lock().unwrap() = None;
        release.notify_waiters();
        settle_replies(&k, WS, "chan:sp_edit", &m1, 1).await;
        settle_replies(&k, WS, "chan:sp_edit", &m2, 1).await;
        report.case(
            "core.queued.08_edit_guest_space_rejected",
            &[("guest_edit_rejected", !edited)],
            &[],
        );
    }

    // 09: Withdraw in guest space rejected
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *host.block_answer.lock().unwrap() = Some((entered.clone(), release.clone()));
        k.core.set_conv_host(host.clone());
        let conv = Conv {
            space: "chan:sp_wd".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let m1 = k
            .core
            .say_in(WS, conv.clone(), "第一条为什么？", &json!({}))
            .unwrap();
        entered.notified().await;
        let m2 = k
            .core
            .say_in(WS, conv, "第二条为什么？", &json!({}))
            .unwrap();
        let wd = k.core.thread_queued_withdraw(WS, &m2).unwrap();
        *host.block_answer.lock().unwrap() = None;
        release.notify_waiters();
        settle_replies(&k, WS, "chan:sp_wd", &m1, 1).await;
        settle_replies(&k, WS, "chan:sp_wd", &m2, 1).await;
        report.case(
            "core.queued.09_withdraw_guest_space_rejected",
            &[("guest_withdraw_rejected", wd.is_none())],
            &[],
        );
    }

    // 10: Edit with wrong workspace rejected, original intact
    {
        let k = replay_kit(true);
        let ws2 = second_ws(&k);
        let (fr, first) = hold_main_turn(&k).await;
        let q = k.core.say(WS, "原空间问题为什么？", &[]).unwrap();
        let wrong_edit = k.core.thread_queued_edit(&ws2, &q, "篡改").unwrap();
        fr.open(2);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &q, 1).await;
        wait_idle_main(&k, WS).await;
        let kept = k.db().message(&q).unwrap().unwrap().text == "原空间问题为什么？";
        report.case(
            "core.queued.10_edit_wrong_workspace_rejected",
            &[
                ("wrong_ws_rejected", !wrong_edit),
                ("original_intact", kept),
            ],
            &[],
        );
    }

    // 11: Edit queued message after ahead/batch preroute triggers Claim::Changed re-route
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let routed_texts = Arc::new(Mutex::new(Vec::<String>::new()));
        let rt = routed_texts.clone();
        k.gw.set(move |p, _| {
            if p.starts_with("You are the router") {
                rt.lock().unwrap().push(p.to_string());
                if p.contains("改为问候") {
                    Ok(json!({"kind": "answer", "confidence": 0.9}).to_string())
                } else {
                    Ok(json!({"kind": "answer", "confidence": 0.9, "after": true}).to_string())
                }
            } else {
                Ok(json!({"thought": "", "answer": "回答修改后内容", "offer": null}).to_string())
            }
        });
        let q = k.core.say(WS, "接着刚才那个说", &[]).unwrap();
        wait_until("deferred onto after_lane", || {
            !routed_texts.lock().unwrap().is_empty()
        })
        .await;
        let edited = k.core.thread_queued_edit(WS, &q, "改为问候").unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &q, 1).await;
        wait_idle_main(&k, WS).await;
        let saw_edited = routed_texts
            .lock()
            .unwrap()
            .iter()
            .any(|p| p.contains("改为问候"));
        report.case(
            "core.queued.11_edit_after_defer_reroutes_edited_text",
            &[
                ("edit_accepted", edited),
                ("rerouted_with_new_text", saw_edited),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &q).len() == 1,
                ),
            ],
            &[],
        );
    }

    // 12: Withdraw queued message must retain durable tombstone (not hard-delete row)
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let q = k.core.say(WS, "墓碑测试为什么？", &[]).unwrap();
        let wd = k.core.thread_queued_withdraw(WS, &q).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        let tombstone_retained = k.db().message(&q).unwrap().is_some();
        report.case(
            "core.queued.12_withdraw_keeps_tombstone_no_hard_delete",
            &[
                ("withdraw_ok", wd.is_some()),
                ("withdrawn_tombstone_no_resurrect", tombstone_retained),
            ],
            &["withdrawn_tombstone_no_resurrect"],
        );
    }
}

// =========================================================================
// 3. Double Claim, Stale Worker Fencing & Concurrent Dispatch (12 cases)
// =========================================================================

async fn run_claim(report: &mut Report) {
    // 01: Two Core instances racing core_queue claim on same pending message -> 1 wins
    {
        let k = replay_kit(false);
        let mid = k.db().message_add(WS, "me", "/汇报", None, None).unwrap();
        let job = json!({
            "mid": mid, "text": "/汇报", "files": [], "user": null,
            "mentions": [], "to": {"item": null, "reply_to": null},
            "conv": {"space": "steve", "research": true, "items": true, "owner_only": false, "start": true, "hands_on": false}
        });
        k.db()
            .store()
            .write(|w| core_queue::enqueue(w, WS, &mid, &job.to_string(), 100))
            .unwrap();
        let c1 = k.reopened();
        let c2 = k.reopened();
        let r1 = c1.thread_ready().unwrap();
        let r2 = c2.thread_ready().unwrap();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid).len();
        report.case(
            "core.claim.01_double_claim_core_queue_two_cores",
            &[
                ("ready_ok", r1.failed == 0 && r2.failed == 0),
                ("le1_final_reply_per_intent", replies == 1),
            ],
            &[],
        );
    }

    // 02: Calling thread_ready twice on same Core for pending message -> 1 reply
    {
        let k = replay_kit(false);
        let mid = k.db().message_add(WS, "me", "/汇报", None, None).unwrap();
        let job = json!({
            "mid": mid, "text": "/汇报", "files": [], "user": null,
            "mentions": [], "to": {"item": null, "reply_to": null},
            "conv": {"space": "steve", "research": true, "items": true, "owner_only": false, "start": true, "hands_on": false}
        });
        k.db()
            .store()
            .write(|w| core_queue::enqueue(w, WS, &mid, &job.to_string(), 100))
            .unwrap();
        let c = k.reopened();
        let _ = c.thread_ready().unwrap();
        let _ = c.thread_ready().unwrap();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid).len();
        report.case(
            "core.claim.02_double_thread_ready_single_core",
            &[("le1_final_reply_per_intent", replies == 1)],
            &[],
        );
    }

    // 03: Two Core instances concurrently calling thread_ready on same interrupted run
    {
        let k = replay_kit(false);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "恢复回答");
        let mid = k
            .db()
            .message_add(WS, "me", "为什么慢？", None, None)
            .unwrap();
        let run = k
            .db()
            .store()
            .write(|w| {
                thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let c1 = k.reopened();
        let c2 = k.reopened();
        let fr1 = ScriptedResearcher::new(RunEnd::Done, "worker1");
        let fr2 = ScriptedResearcher::new(RunEnd::Done, "worker2");
        c1.set_researcher(fr1.clone());
        c2.set_researcher(fr2.clone());
        c1.thread_recover().unwrap();
        // Both cores race thread_ready on the same interrupted run (no CAS on status/attempts!)
        let b = Arc::new(Barrier::new(2));
        let (b1, b2) = (b.clone(), b.clone());
        let (c1c, c2c) = (c1.clone(), c2.clone());
        let h1 = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(b1.wait());
            c1c.thread_ready().unwrap()
        });
        let h2 = tokio::task::spawn_blocking(move || {
            tokio::runtime::Handle::current().block_on(b2.wait());
            c2c.thread_ready().unwrap()
        });
        let (r1, r2) = (h1.await.unwrap(), h2.await.unwrap());
        fr1.open();
        fr2.open();
        wait_until("resumed replies written", || {
            let total_seen = fr1.seen.lock().unwrap().len() + fr2.seen.lock().unwrap().len();
            total_seen >= (r1.resumed + r2.resumed)
                && steve_replies_for(&k, WS, MAIN, &mid).len() >= 1 + total_seen
        })
        .await;
        let _ = run;
        let execs = fr1.seen.lock().unwrap().len() + fr2.seen.lock().unwrap().len();
        let replies = steve_replies_for(&k, WS, MAIN, &mid).len();
        report.case(
            "core.claim.03_double_thread_ready_two_cores_interrupted_run",
            &[
                ("double_claim_single_execution", execs <= 1),
                ("le1_final_reply_per_intent", replies <= 1),
            ],
            &[
                "double_claim_single_execution",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 04: Stale worker finishes late after recover + restart -> must be fenced
    {
        let k = replay_kit(false);
        let r1 = ScriptedResearcher::new(RunEnd::Done, "旧worker迟到答案");
        k.core.set_researcher(r1.clone());
        let mid = k.core.say(WS, "旧worker测试为什么？", &[]).unwrap();
        r1.wait_entered().await;
        // Simulate Hub restart while old worker is still alive in-flight
        let k2 = k.reopened();
        let r2 = ScriptedResearcher::new(RunEnd::Done, "新worker答案");
        k2.set_researcher(r2.clone());
        k2.thread_recover().unwrap();
        k2.thread_ready().unwrap();
        r2.wait_entered().await;
        r2.open();
        wait_until("new worker committed", || {
            steve_replies_for(&k, WS, MAIN, &mid)
                .iter()
                .any(|m| m.text.contains("新worker答案"))
        })
        .await;
        // Now release stale worker 1; it must NOT write a second final reply!
        r1.open();
        wait_until("stale worker exited", || {
            r1.in_flight.load(Ordering::SeqCst) == 0
        })
        .await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        let stale_wrote = replies.iter().any(|m| m.text.contains("旧worker迟到答案"));
        report.case(
            "core.claim.04_stale_worker_commits_after_recover_restart",
            &[
                ("stale_worker_fenced", !stale_wrote),
                ("le1_final_reply_per_intent", replies.len() <= 1),
            ],
            &["stale_worker_fenced", "le1_final_reply_per_intent"],
        );
    }

    // 05: Concurrent offer take on same Core -> 1 item
    {
        let k = replay_kit(true);
        let origin = k
            .db()
            .message_add(WS, "me", "修评论吗？", None, None)
            .unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "可以修",
                None,
                Some(&json!({"origin": origin, "offer": {"title": "修评论", "goal": "支持markdown", "state": "open"}})),
            )
            .unwrap();
        let h1 = k.core.offer(WS, &reply, true, None).unwrap();
        let h2 = k.core.offer(WS, &reply, true, None).unwrap();
        report.case(
            "core.claim.05_concurrent_offer_same_core",
            &[
                ("first_created", !h1.already && h1.item.is_some()),
                ("second_already", h2.already),
                (
                    "le1_dispatch_per_intent",
                    k.db().items(WS).unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 06: Offer take across two Core instances (no DB unique constraint on origin_msg dispatch)
    {
        let k = replay_kit(true);
        let origin = k
            .db()
            .message_add(WS, "me", "修评论吗？", None, None)
            .unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "可以修",
                None,
                Some(&json!({"origin": origin, "offer": {"title": "修评论", "goal": "支持markdown", "state": "open"}})),
            )
            .unwrap();
        // Simulate a stale message snapshot where a second offer message exists for the same origin
        let reply2 = k
            .db()
            .message_add(
                WS,
                "steve",
                "重复提议",
                None,
                Some(&json!({"origin": origin, "offer": {"title": "修评论", "goal": "支持markdown", "state": "open"}})),
            )
            .unwrap();
        let c1 = k.reopened();
        let c2 = k.reopened();
        let _ = c1.offer(WS, &reply, true, None).unwrap();
        let _ = c2.offer(WS, &reply2, true, None).unwrap();
        let items = k.db().items(WS).unwrap().len();
        report.case(
            "core.claim.06_two_offers_same_origin_single_dispatch",
            &[("le1_dispatch_per_intent", items <= 1)],
            &["le1_dispatch_per_intent"],
        );
    }

    // 07: Double suggest(item) on same Core -> 1 item
    {
        let k = replay_kit(true);
        let origin = k
            .db()
            .message_add(WS, "me", "整理页面", None, None)
            .unwrap();
        let card = k
            .db()
            .message_add(
                WS,
                "steve",
                "建项吗？",
                None,
                Some(&json!({
                    "origin": origin,
                    "suggest": {
                        "state": "open",
                        "choices": [
                            {"id": "here", "label": "在这答"},
                            {"id": "item", "label": "建项", "title": "整理", "goal": "整理页面"}
                        ]
                    }
                })),
            )
            .unwrap();
        let h1 = k.core.suggest(WS, &card, "item", None).unwrap();
        let h2 = k.core.suggest(WS, &card, "item", None).unwrap();
        report.case(
            "core.claim.07_concurrent_suggest_item_same_core",
            &[
                ("first_created", !h1.already),
                ("second_already", h2.already),
                (
                    "le1_dispatch_per_intent",
                    k.db().items(WS).unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 08: Concurrent promote on same Core -> 1 item via OnceCell
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "转化", "goal": "转化目标"}),
            "ok",
        );
        let origin = k.db().message_add(WS, "me", "原问题", None, None).unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "原回答",
                None,
                Some(&json!({"origin": origin, "chat": "原问题"})),
            )
            .unwrap();
        let (r1, r2) = tokio::join!(
            k.core.promote(WS, &reply, None),
            k.core.promote(WS, &reply, None),
        );
        let (i1, i2) = (r1.unwrap(), r2.unwrap());
        report.case(
            "core.claim.08_concurrent_promote_same_core",
            &[
                ("same_item_returned", i1.id == i2.id),
                (
                    "le1_dispatch_per_intent",
                    k.db().items(WS).unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 09: Concurrent promote across TWO Core instances -> in-memory OnceCell does not protect!
    {
        let k = replay_kit(true);
        let gate = Arc::new(Barrier::new(2));
        let g = gate.clone();
        k.gw.set(move |_, _| {
            // Both cores enter router concurrently before either writes meta.promoted.
            // Bounded timeout ensures future W3.4 single-entry CAS never deadlocks here.
            let g2 = g.clone();
            tokio::task::block_in_place(|| {
                let _ = tokio::runtime::Handle::current()
                    .block_on(tokio::time::timeout(Duration::from_millis(200), g2.wait()));
            });
            Ok(json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "双提升", "goal": "双提升目标"}).to_string())
        });
        let origin = k.db().message_add(WS, "me", "原问题", None, None).unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "原回答",
                None,
                Some(&json!({"origin": origin, "chat": "原问题"})),
            )
            .unwrap();
        let c1 = k.reopened();
        let c2 = k.reopened();
        let (r1_id, r2_id) = (reply.clone(), reply.clone());
        let t1 = tokio::spawn(async move { c1.promote(WS, &r1_id, None).await });
        let t2 = tokio::spawn(async move { c2.promote(WS, &r2_id, None).await });
        let _ = (t1.await.unwrap().unwrap(), t2.await.unwrap().unwrap());
        let items = k.db().items(WS).unwrap().len();
        report.case(
            "core.claim.09_concurrent_promote_two_cores",
            &[("le1_dispatch_per_intent", items <= 1)],
            &["le1_dispatch_per_intent"],
        );
    }

    // 10: Concurrent hand_off + promote for same origin -> no DB unique dispatch key per origin
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "转项", "goal": "查磁盘"}),
            "ok",
        );
        let origin = k.db().message_add(WS, "me", "查磁盘", None, None).unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "先答一下",
                None,
                Some(&json!({"origin": origin, "chat": "查磁盘"})),
            )
            .unwrap();
        let _ = k.core.promote(WS, &reply, None).await.unwrap();
        // Second explicit reroute(Item) on user message when route_log was not pre-seeded
        let _ = k
            .core
            .reroute(WS, &origin, RerouteTo::Item, None)
            .await
            .unwrap();
        let items = k.db().items(WS).unwrap().len();
        report.case(
            "core.claim.10_promote_then_reroute_item_same_origin",
            &[("le1_dispatch_per_intent", items <= 1)],
            &["le1_dispatch_per_intent"],
        );
    }

    // 11: Dismiss offer then try to take -> already=true, 0 items
    {
        let k = replay_kit(true);
        let origin = k.db().message_add(WS, "me", "改吗", None, None).unwrap();
        let reply = k
            .db()
            .message_add(
                WS,
                "steve",
                "要改吗",
                None,
                Some(&json!({"origin": origin, "offer": {"title": "改", "goal": "改", "state": "open"}})),
            )
            .unwrap();
        let d1 = k.core.offer(WS, &reply, false, None).unwrap();
        let d2 = k.core.offer(WS, &reply, true, None).unwrap();
        report.case(
            "core.claim.11_offer_take_after_dismiss_rejected",
            &[
                ("dismiss_ok", !d1.already),
                ("take_after_dismiss_already", d2.already),
                ("zero_items", k.db().items(WS).unwrap().is_empty()),
            ],
            &[],
        );
    }

    // 12: Suggest chosen "here", then conflicting "item" -> already=true, 0 items
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("方案")]);
        k.core.set_researcher(fr);
        let origin = k
            .db()
            .message_add(WS, "me", "怎么设计", None, None)
            .unwrap();
        let card = k
            .db()
            .message_add(
                WS,
                "steve",
                "怎么选？",
                None,
                Some(&json!({
                    "origin": origin,
                    "suggest": {
                        "state": "open",
                        "choices": [
                            {"id": "here", "label": "在这出方案"},
                            {"id": "item", "label": "建项", "title": "设计", "goal": "设计"}
                        ]
                    }
                })),
            )
            .unwrap();
        let h1 = k.core.suggest(WS, &card, "here", None).unwrap();
        let h2 = k.core.suggest(WS, &card, "item", None).unwrap();
        settle_replies(&k, WS, MAIN, &origin, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.claim.12_suggest_different_choice_after_chosen_rejected",
            &[
                ("first_here_ok", !h1.already),
                ("second_item_already", h2.already),
                ("zero_items", k.db().items(WS).unwrap().is_empty()),
            ],
            &[],
        );
    }
}

// =========================================================================
// 4. ACL / Guest Isolation & Scope Boundaries (12 cases)
// =========================================================================

async fn run_acl(report: &mut Report) {
    // 01: Guest blocks config, supplement, status, reroute, ask
    {
        let k = replay_kit(true);
        let existing_item = k.item("run", "执行", None);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        let kinds = ["config", "supplement", "status", "reroute", "ask"];
        let mut all_answer = true;
        for (idx, kind) in kinds.iter().enumerate() {
            set_scripted_gateway(
                &k,
                json!({"kind": kind, "confidence": 0.95, "item": existing_item, "reply": "问一下"}),
                "ok",
            );
            let conv = Conv {
                space: format!("chan:sp_g1_{idx}"),
                research: false,
                items: false,
                owner_only: false,
                start: false,
                hands_on: false,
            };
            let mid = k
                .core
                .say_in(WS, conv.clone(), "执行操作", &json!({}))
                .unwrap();
            settle_replies(&k, WS, &conv.space, &mid, 1).await;
            let rl = k.db().store().thread().route_of(&mid).unwrap().unwrap();
            if rl.kind != "answer" || rl.guard.as_deref() != Some("guest") {
                all_answer = false;
            }
        }
        report.case(
            "core.acl.01_guest_blocks_config_supplement_status_reroute",
            &[
                ("all_downgraded_to_guest_answer", all_answer),
                (
                    "zero_new_items_created",
                    k.db().items(WS).unwrap().len() == 1,
                ),
            ],
            &[],
        );
    }

    // 02: Guest cannot invoke /汇报 workspace digest
    {
        let k = replay_kit(true);
        k.item("run", "执行", None);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let conv = Conv {
            space: "chan:sp_digest".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k.core.say_in(WS, conv, "/汇报", &json!({})).unwrap();
        let replies = settle_replies(&k, WS, "chan:sp_digest", &mid, 1).await;
        let leaked = replies[0].text.contains("件在进行");
        report.case(
            "core.acl.02_guest_blocks_digest_slash_command",
            &[("zero_digest_leak", !leaked)],
            &[],
        );
    }

    // 03: Guest with items=false downgrades item route to inline offer, 0 items
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["explicit", "change"], "title": "改代码", "goal": "修bug"}),
            "ok",
        );
        let conv = Conv {
            space: "chan:sp_noitem".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(WS, conv, "建个事项修bug", &json!({}))
            .unwrap();
        let replies = settle_replies(&k, WS, "chan:sp_noitem", &mid, 1).await;
        report.case(
            "core.acl.03_guest_items_disabled_downgrades_to_offer",
            &[
                ("zero_items_created", k.db().items(WS).unwrap().is_empty()),
                (
                    "has_offer_meta",
                    replies[0].meta["offer"]["state"] == "open",
                ),
            ],
            &[],
        );
    }

    // 04: Guest with research=false makes 0 researcher invocations
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("不应运行")]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        let conv = Conv {
            space: "chan:sp_nores".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(WS, conv, "深入查一下代码架构", &json!({}))
            .unwrap();
        settle_replies(&k, WS, "chan:sp_nores", &mid, 1).await;
        report.case(
            "core.acl.04_guest_research_disabled_zero_researcher",
            &[("zero_researcher_calls", fr.seen.lock().unwrap().is_empty())],
            &[],
        );
    }

    // 05: Guest with items=true, start=false creates proposal (not started)
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["explicit", "change"], "title": "客户需求", "goal": "加导出"}),
            "ok",
        );
        let conv = Conv {
            space: "chan:sp_prop".into(),
            research: true,
            items: true,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(WS, conv, "建个事项加导出", &json!({}))
            .unwrap();
        settle_replies(&k, WS, "chan:sp_prop", &mid, 1).await;
        let items = k.db().items(WS).unwrap();
        let is_propose = items.len() == 1 && items[0].status == "wait" && items[0].stage == "规划";
        let not_started = k.rt.st().prompts.is_empty();
        report.case(
            "core.acl.05_guest_non_owner_item_stays_proposal",
            &[
                ("item_is_proposal", is_propose),
                ("not_auto_started", not_started),
            ],
            &[],
        );
    }

    // 06: Guest research (owner_only=false) has no projects, no private items, full=false, ui=false, guest context present
    {
        let k = replay_kit(true);
        let secret_item = k.item("run", "执行", None);
        let _ = k
            .db()
            .project_create(WS, "PrivateProj", "", "/tmp/p", "dir", false, None, None)
            .unwrap();
        let fr = FakeResearch::new(vec![Step::Text("查完了")]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        let conv = Conv {
            space: "chan:sp_scope".into(),
            research: true,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(
                WS,
                conv,
                &format!("查一下 {secret_item} 的详情"),
                &json!({"chan": {"owner_only": false}}),
            )
            .unwrap();
        settle_replies(&k, WS, "chan:sp_scope", &mid, 1).await;
        let req = fr.seen.lock().unwrap()[0].clone();
        report.case(
            "core.acl.06_guest_research_no_projects_no_named_items_no_full",
            &[
                (
                    "no_readable_projects",
                    req.readable.is_empty() && req.project.is_none(),
                ),
                ("not_full_not_ui", !req.full && !req.ui),
                (
                    "no_private_item_injected",
                    !req.context.contains("对话提到的事项"),
                ),
                (
                    "guest_scope_context_present",
                    req.context.contains("GUEST_SCOPE_CONTEXT_OPS"),
                ),
            ],
            &[],
        );
    }

    // 07: Guest owner_only=true grants full ONLY when asked by owner, not non-owner member
    {
        let k = replay_kit(true);
        let users = k.db().store().users();
        let owner = users.create("local:o7", "Owner", None, "owner").unwrap();
        let member = users.create("local:m7", "Member", None, "member").unwrap();
        let fr = FakeResearch::new(vec![Step::Text("ok")]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let conv = Conv {
            space: "chan:sp_own".into(),
            research: true,
            items: true,
            owner_only: true,
            start: true,
            hands_on: false,
        };
        let m_mem = k
            .core
            .say_in_as(
                WS,
                conv.clone(),
                "看看电脑",
                &json!({"chan": {"owner_only": true}}),
                Some(&member),
            )
            .unwrap();
        settle_replies(&k, WS, "chan:sp_own", &m_mem, 1).await;
        let m_own = k
            .core
            .say_in_as(
                WS,
                conv,
                "再看看电脑",
                &json!({"chan": {"owner_only": true}}),
                Some(&owner),
            )
            .unwrap();
        settle_replies(&k, WS, "chan:sp_own", &m_own, 1).await;
        let seen = fr.seen.lock().unwrap();
        let mem_full = seen.iter().find(|r| r.mid == m_mem).unwrap().full;
        let own_full = seen.iter().find(|r| r.mid == m_own).unwrap().full;
        report.case(
            "core.acl.07_guest_owner_only_full_requires_owner",
            &[("member_not_full", !mem_full), ("owner_is_full", own_full)],
            &[],
        );
    }

    // 08: Missing origin ID must NOT fall back to MAIN space and leak private MAIN history
    {
        let k = replay_kit(true);
        k.db()
            .message_add(WS, "me", "MAIN_SECRET_NOTE_999", None, None)
            .unwrap();
        let space = k.core.conv_of("msg_does_not_exist");
        let hist = k.core.conv_messages(WS, &space).unwrap();
        let leaked = space == MAIN || hist.iter().any(|m| m.text.contains("MAIN_SECRET_NOTE_999"));
        report.case(
            "core.acl.08_missing_origin_no_main_space_fallback",
            &[("missing_origin_no_main_fallback", !leaked)],
            &["missing_origin_no_main_fallback"],
        );
    }

    // 09: Non-owner member in MAIN gets full=false and no onboard brief/tools
    {
        let k = replay_kit(true);
        let users = k.db().store().users();
        let _owner = users.create("local:o9", "Owner", None, "owner").unwrap();
        let member = users.create("local:m9", "Member", None, "member").unwrap();
        k.core.onboard_start(WS, "owner").unwrap();
        let fr = FakeResearch::new(vec![Step::Text("ok")]);
        k.core.set_researcher(fr.clone());
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.9}), "ok");
        let mid = k
            .core
            .say_as(WS, Some(&member), "为什么慢？", &[], &[])
            .unwrap();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let req = fr.seen.lock().unwrap()[0].clone();
        let allowed = k.core.onboard_allows(WS, &mid, Some(&member)).unwrap();
        report.case(
            "core.acl.09_non_owner_member_in_main_no_full_no_onboard",
            &[
                ("member_not_full", !req.full),
                ("no_onboard_brief", req.brief.is_empty()),
                ("onboard_disallowed", !allowed),
            ],
            &[],
        );
    }

    // 10: suggest("here", Some(member)) must preserve caller user_id on run/request
    {
        let k = replay_kit(true);
        let users = k.db().store().users();
        let _owner = users.create("local:o10", "Owner", None, "owner").unwrap();
        let member = users.create("local:m10", "Member", None, "member").unwrap();
        let fr = FakeResearch::new(vec![Step::Text("方案来了")]);
        k.core.set_researcher(fr.clone());
        let origin = k
            .db()
            .message_add(WS, "me", "怎么设计", None, None)
            .unwrap();
        let card = k
            .db()
            .message_add(
                WS,
                "steve",
                "怎么选？",
                None,
                Some(&json!({
                    "origin": origin,
                    "suggest": {"state": "open", "choices": [{"id": "here", "label": "在这答"}]}
                })),
            )
            .unwrap();
        k.core.suggest(WS, &card, "here", Some(&member)).unwrap();
        settle_replies(&k, WS, MAIN, &origin, 1).await;
        wait_idle_main(&k, WS).await;
        let req_user = fr.seen.lock().unwrap()[0].user.clone();
        let run_user = k.db().store().thread().runs_of_message(&origin).unwrap()[0]
            .user_id
            .clone();
        report.case(
            "core.acl.10_suggest_here_preserves_caller_user_id",
            &[(
                "caller_identity_preserved",
                req_user.as_deref() == Some(member.as_str())
                    && run_user.as_deref() == Some(member.as_str()),
            )],
            &["caller_identity_preserved"],
        );
    }

    // 11: reroute(Inline, Some(member)) must preserve caller user_id on run/request
    {
        let k = replay_kit(true);
        let users = k.db().store().users();
        let _owner = users.create("local:o11", "Owner", None, "owner").unwrap();
        let member = users.create("local:m11", "Member", None, "member").unwrap();
        let fr = FakeResearch::new(vec![Step::Text("改在这答")]);
        k.core.set_researcher(fr.clone());
        let origin = k
            .db()
            .message_add(WS, "me", "解释架构", None, None)
            .unwrap();
        k.core
            .reroute(WS, &origin, RerouteTo::Inline, Some(&member))
            .await
            .unwrap();
        settle_replies(&k, WS, MAIN, &origin, 1).await;
        wait_idle_main(&k, WS).await;
        let req_user = fr.seen.lock().unwrap()[0].user.clone();
        let run_user = k.db().store().thread().runs_of_message(&origin).unwrap()[0]
            .user_id
            .clone();
        report.case(
            "core.acl.11_reroute_inline_preserves_caller_user_id",
            &[(
                "caller_identity_preserved",
                req_user.as_deref() == Some(member.as_str())
                    && run_user.as_deref() == Some(member.as_str()),
            )],
            &["caller_identity_preserved"],
        );
    }

    // 12: Cross-workspace reroute/offer/suggest/promote/a2ui all fail closed
    {
        let k = replay_kit(true);
        let ws2 = second_ws(&k);
        let origin = k.db().message_add(&ws2, "me", "秘密", None, None).unwrap();
        let msg2 = k
            .db()
            .message_add(
                &ws2,
                "steve",
                "回答\n```a2ui\n{\"surfaceId\":\"s\"}\n```",
                None,
                Some(&json!({
                    "origin": origin,
                    "chat": "秘密",
                    "offer": {"title": "t", "goal": "g", "state": "open"},
                    "suggest": {"state": "open", "choices": [{"id": "item", "label": "建", "title": "t", "goal": "g"}]}
                })),
            )
            .unwrap();
        let e1 = k.core.offer(WS, &msg2, true, None).is_err();
        let e2 = k.core.suggest(WS, &msg2, "item", None).is_err();
        let e3 = k.core.promote(WS, &msg2, None).await.is_err();
        let e4 = k
            .core
            .reroute(WS, &msg2, RerouteTo::Inline, None)
            .await
            .is_err();
        let e5 = k
            .core
            .a2ui_action(
                WS,
                None,
                &msg2,
                A2uiAction {
                    block: "b1".into(),
                    surface: "s".into(),
                    component: "c".into(),
                    name: "n".into(),
                    label: "l".into(),
                    context: json!({}),
                    data: json!({}),
                    timestamp: "t".into(),
                },
            )
            .is_err();
        report.case(
            "core.acl.12_cross_workspace_actions_fail_closed",
            &[
                ("all_cross_ws_rejected", e1 && e2 && e3 && e4 && e5),
                ("zero_foreign_items", k.db().items(&ws2).unwrap().is_empty()),
            ],
            &[],
        );
    }
}

// =========================================================================
// 5. Cancel / Stop & Late Results (12 cases)
// =========================================================================

async fn run_cancel(report: &mut Report) {
    // 01: Stop during slow router -> 1 stop reply, 0 inline runs, 0 items
    {
        let k = replay_kit(true);
        let entered = Arc::new(Notify::new());
        let e2 = entered.clone();
        k.gw.set(move |p, _| {
            if p.starts_with("You are the router") {
                e2.notify_one();
                Ok(json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "迟到", "goal": "迟到"}).to_string())
            } else {
                Ok("ok".into())
            }
        });
        *k.gw.slow.lock().unwrap() = Some(("You are the router", Duration::from_secs(5)));
        let mid = k.core.say(WS, "慢路由测试", &[]).unwrap();
        wait_until("router inflight", || k.core.routing(WS)).await;
        let stopped = k.core.stop(WS).unwrap();
        wait_idle_main(&k, WS).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.cancel.01_stop_during_routing_zero_inline_zero_item",
            &[
                ("stop_returned_true", stopped),
                ("le1_final_reply_per_intent", replies.len() == 1),
                ("zero_items_created", k.db().items(WS).unwrap().is_empty()),
            ],
            &[],
        );
    }

    // 02: Calling stop() twice rapidly while routing -> must write at most 1 stop receipt!
    {
        let k = replay_kit(true);
        *k.gw.slow.lock().unwrap() = Some(("You are the router", Duration::from_secs(5)));
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "ok");
        let mid = k.core.say(WS, "双停测试", &[]).unwrap();
        wait_until("router inflight", || k.core.routing(WS)).await;
        let s1 = k.core.stop(WS).unwrap();
        let s2 = k.core.stop(WS).unwrap();
        wait_idle_main(&k, WS).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.cancel.02_stop_twice_during_routing_single_receipt",
            &[
                ("first_stop_ok", s1),
                ("stop_idempotent_single_receipt", !s2 && replies.len() == 1),
                ("le1_final_reply_per_intent", replies.len() <= 1),
            ],
            &[
                "stop_idempotent_single_receipt",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 03: Stop queued inline run before it starts -> 0 researcher calls for queued, 1 stop reply
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("结果"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        let m1 = k.core.say(WS, "问题1为什么？", &[]).unwrap();
        wait_until("m1 running", || !fr.seen.lock().unwrap().is_empty()).await;
        let m2 = k
            .db()
            .message_add(WS, "me", "问题2为什么？", None, None)
            .unwrap();
        let card = k
            .db()
            .message_add(
                WS,
                "steve",
                "怎么选？",
                None,
                Some(&json!({
                    "origin": m2,
                    "suggest": {"state": "open", "choices": [{"id": "here", "label": "在这答"}]}
                })),
            )
            .unwrap();
        k.core.suggest(WS, &card, "here", None).unwrap();
        // Wait until m2 enters inline queue (thread_runs row queued)
        let mut run2 = String::new();
        wait_until("m2 queued as thread_run", || {
            let runs = k.db().store().thread().runs_of_message(&m2).unwrap();
            if let Some(r) = runs.first() {
                run2 = r.id.clone();
                true
            } else {
                false
            }
        })
        .await;
        let stopped = k.core.stop_run(WS, &run2).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &m1, 1).await;
        // m2 has the card + stop reply (2 messages with origin=m2); verify 1 stop reply & 0 execution
        settle_replies(&k, WS, MAIN, &m2, 2).await;
        wait_idle_main(&k, WS).await;
        let ran_m2 = fr.seen.lock().unwrap().iter().any(|r| r.mid == m2);
        let r2_status = k.db().store().thread().run(&run2).unwrap().unwrap().status;
        let stop_replies = steve_replies_for(&k, WS, MAIN, &m2)
            .into_iter()
            .filter(|m| m.id != card)
            .count();
        report.case(
            "core.cancel.03_stop_during_queued_inline_zero_execution",
            &[
                ("stop_run_ok", stopped),
                ("queued_zero_execution", !ran_m2),
                ("status_stopped", r2_status == "stopped"),
                ("le1_final_reply_per_intent", stop_replies == 1),
            ],
            &[],
        );
    }

    // 04: Repeated stop_run on same queued run is idempotent
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("结果"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        let m1 = k.core.say(WS, "问题1为什么？", &[]).unwrap();
        wait_until("m1 running", || !fr.seen.lock().unwrap().is_empty()).await;
        let m2 = k
            .db()
            .message_add(WS, "me", "问题2为什么？", None, None)
            .unwrap();
        let card = k
            .db()
            .message_add(
                WS,
                "steve",
                "怎么选？",
                None,
                Some(&json!({
                    "origin": m2,
                    "suggest": {"state": "open", "choices": [{"id": "here", "label": "在这答"}]}
                })),
            )
            .unwrap();
        k.core.suggest(WS, &card, "here", None).unwrap();
        let mut run2 = String::new();
        wait_until("m2 run queued", || {
            let runs = k.db().store().thread().runs_of_message(&m2).unwrap();
            runs.first().is_some_and(|r| {
                run2 = r.id.clone();
                true
            })
        })
        .await;
        let s1 = k.core.stop_run(WS, &run2).unwrap();
        let s2 = k.core.stop_run(WS, &run2).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &m1, 1).await;
        wait_idle_main(&k, WS).await;
        let stop_replies = steve_replies_for(&k, WS, MAIN, &m2)
            .into_iter()
            .filter(|m| m.id != card)
            .count();
        report.case(
            "core.cancel.04_stop_run_twice_idempotent",
            &[
                ("first_true_second_false", s1 && !s2),
                ("le1_final_reply_per_intent", stop_replies == 1),
            ],
            &[],
        );
    }

    // 05: Stop during answer streaming keeps partial/stop receipt and emits 0 drafts after stop
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "answer", "confidence": 0.9}),
            "很长的回答",
        );
        *k.gw.slow.lock().unwrap() = Some(("You are Steve", Duration::from_secs(5)));
        let mid = k.core.say(WS, "慢慢回答我", &[]).unwrap();
        wait_until("answer running", || answer_calls(&k) >= 1).await;
        let stopped = k.core.stop(WS).unwrap();
        let events_at_stop = k.events.lock().unwrap().len();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let late_draft = k.events.lock().unwrap()[events_at_stop..].iter().any(|e| {
            matches!(
                e,
                crate::repo::BusEvent::Live(steve_types::LiveEvent::Work(
                    steve_types::WorkLiveEvent::SteveDraft { .. }
                ))
            )
        });
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.cancel.05_stop_during_answer_no_late_draft",
            &[
                ("stop_ok", stopped),
                ("zero_late_drafts", !late_draft),
                ("marked_stopped", replies[0].meta["stopped"] == json!(true)),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 06: Stop during research overrides late RunEnd::Done to Stopped
    {
        let k = replay_kit(true);
        let r = Arc::new(ScriptedResearcher {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: true,
            outcome_end: RunEnd::Done,
            outcome_text: "迟到正文",
            outcome_offer: None,
            outcome_error: None,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么这么慢？", &[]).unwrap();
        r.wait_entered().await;
        let stopped = k.core.stop(WS).unwrap();
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let run = k
            .db()
            .store()
            .thread()
            .runs_of_message(&mid)
            .unwrap()
            .pop()
            .unwrap();
        report.case(
            "core.cancel.06_stop_during_research_drops_late_done",
            &[
                ("stop_ok", stopped),
                ("run_status_stopped", run.status == "stopped"),
                ("reply_marked_stopped", replies[0].text.contains("已停下")),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 07: Stop during research must NOT attach a late Offer to the stopped reply!
    {
        let k = replay_kit(true);
        let r = Arc::new(ScriptedResearcher {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: true,
            outcome_end: RunEnd::Done,
            outcome_text: "迟到正文",
            outcome_offer: Some(crate::gateway::Offer {
                title: "迟到提议".into(),
                goal: "不该出现".into(),
            }),
            outcome_error: None,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么这么慢？", &[]).unwrap();
        r.wait_entered().await;
        k.core.stop(WS).unwrap();
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let has_late_offer = replies[0].meta.get("offer").is_some();
        report.case(
            "core.cancel.07_stop_during_research_drops_late_offer",
            &[("stopped_no_late_offer", !has_late_offer)],
            &["stopped_no_late_offer"],
        );
    }

    // 08: Stop during guest research marks host_reply ineligible (0 external send)
    {
        let k = replay_kit(true);
        let r = Arc::new(ScriptedResearcher {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: true,
            outcome_end: RunEnd::Done,
            outcome_text: "访客迟到结果",
            outcome_offer: None,
            outcome_error: None,
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        k.core.set_researcher(r.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        let conv = Conv {
            space: "chan:sp_gstop".into(),
            research: true,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(
                WS,
                conv,
                "帮我深入查一下当前服务的部署状态和运行配置",
                &json!({}),
            )
            .unwrap();
        r.wait_entered().await;
        let run_id = k.db().store().thread().runs_of_message(&mid).unwrap()[0]
            .id
            .clone();
        k.core.stop_run(WS, &run_id).unwrap();
        r.open();
        let replies = settle_replies(&k, WS, "chan:sp_gstop", &mid, 1).await;
        report.case(
            "core.cancel.08_stop_during_guest_research_marks_ineligible",
            &[
                (
                    "marked_ineligible",
                    replies[0].meta["host_reply"]["status"] == "ineligible",
                ),
                (
                    "not_prepared_for_send",
                    host.research_prepared.lock().unwrap().is_empty(),
                ),
            ],
            &[],
        );
    }

    // 09: Stop during guest_answer (awaiting host.answer) should cancel promptly via select!
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *host.block_answer.lock().unwrap() = Some((entered.clone(), release.clone()));
        k.core.set_conv_host(host.clone());
        let conv = Conv {
            space: "chan:sp_ga_stop".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k.core.say_in(WS, conv, "为什么慢？", &json!({})).unwrap();
        entered.notified().await;
        let run_id = k.db().store().thread().runs_of_message(&mid).unwrap()[0]
            .id
            .clone();
        let stopped = k.core.stop_run(WS, &run_id).unwrap();
        // Check whether guest_answer unblocks within 60ms without releasing host.answer!
        let prompt_cancel = tokio::time::timeout(
            Duration::from_millis(60),
            settle_replies(&k, WS, "chan:sp_ga_stop", &mid, 1),
        )
        .await
        .is_ok();
        release.notify_waiters();
        let replies = settle_replies(&k, WS, "chan:sp_ga_stop", &mid, 1).await;
        report.case(
            "core.cancel.09_stop_during_guest_answer_cancels_promptly",
            &[
                ("stop_ok", stopped),
                ("guest_stop_cancels_in_flight", prompt_cancel),
                (
                    "reply_marked_stopped",
                    replies[0].meta["stopped"] == json!(true),
                ),
            ],
            &["guest_stop_cancels_in_flight"],
        );
    }

    // 10: Stop during slow research followed by late completion does not trigger chat_done side effects
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("一半"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        let mid = k.core.say(WS, "为什么卡？", &[]).unwrap();
        wait_until("running", || !fr.seen.lock().unwrap().is_empty()).await;
        k.core.stop(WS).unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let run = k
            .db()
            .store()
            .thread()
            .runs_of_message(&mid)
            .unwrap()
            .pop()
            .unwrap();
        report.case(
            "core.cancel.10_stop_during_research_commits_single_stopped_receipt",
            &[
                ("run_stopped", run.status == "stopped"),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 11: Explicit reroute(Item) while inline research is running must fence stale research reply!
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("旧研究答案"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["change"], "title": "改建项", "goal": "转成事项"}),
            "ok",
        );
        let mid = k.core.say(WS, "为什么内存高？", &[]).unwrap();
        wait_until("research running", || !fr.seen.lock().unwrap().is_empty()).await;
        // User explicitly reroutes the message to an item while research is still in-flight
        let _ = k
            .core
            .reroute(WS, &mid, RerouteTo::Item, None)
            .await
            .unwrap();
        // Now old research finishes late -> must NOT commit a second reply for the same origin!
        fr.open(1);
        wait_idle_main(&k, WS).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        let stale_research_committed = replies.iter().any(|m| m.text.contains("旧研究答案"));
        report.case(
            "core.cancel.11_reroute_fences_running_inline_reply",
            &[
                ("reroute_fences_stale_reply", !stale_research_committed),
                ("le1_final_reply_per_intent", replies.len() <= 1),
            ],
            &["reroute_fences_stale_reply", "le1_final_reply_per_intent"],
        );
    }

    // 12: Stopping turn 1 leaves held turn 2 to execute cleanly
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let second = k.core.say(WS, "第二条为什么？", &[]).unwrap();
        k.core.stop(WS).unwrap();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &second, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.cancel.12_stop_does_not_cancel_waiting_held_message",
            &[
                (
                    "first_stopped_once",
                    steve_replies_for(&k, WS, MAIN, &first).len() == 1,
                ),
                (
                    "second_answered_once",
                    steve_replies_for(&k, WS, MAIN, &second).len() == 1,
                ),
            ],
            &[],
        );
    }
}

// =========================================================================
// 6. Stream Interruption, Partial Output & Gateway Timeouts (10 cases)
// =========================================================================

async fn run_stream(report: &mut Report) {
    // 01: Streaming error after partial draft -> draft never in unread, final reply marked error
    {
        let dir = tempfile::tempdir().unwrap();
        let mut cfg = crate::CoreConfig::for_home(dir.path());
        cfg.no_resume = true;
        cfg.port = 18799;
        for d in [&cfg.data_dir, &cfg.worktrees, &cfg.scratch] {
            std::fs::create_dir_all(d).unwrap();
        }
        let raw = steve_store::Store::open(
            dir.path().join("data/steve.sqlite3"),
            steve_store::NoPublisher,
        )
        .unwrap();
        let store = crate::repo::Db::new(raw);
        let rt = Arc::new(crate::testkit::FakeRuntime {
            store: store.clone(),
            s: Default::default(),
            flushed: Notify::new(),
            stop_gate: std::sync::RwLock::new(()),
        });
        let gw = Arc::new(FailingStreamGateway {
            calls: AtomicUsize::new(0),
        });
        let core = crate::Core::new(store.clone(), rt, gw.clone(), cfg);
        core.seed().unwrap();
        let user = store
            .store()
            .users()
            .create("local:su1", "U1", None, "owner")
            .unwrap();
        let mid = core
            .say_as(WS, Some(&user), "为什么断流？", &[], &[])
            .unwrap();
        let mut replies = vec![];
        wait_until("failed stream reply", || {
            replies = store
                .messages_in(WS, MAIN, 200)
                .unwrap()
                .into_iter()
                .filter(|m| m.author == "steve" && m.meta["origin"].as_str() == Some(mid.as_str()))
                .collect();
            !replies.is_empty()
        })
        .await;
        let run = store
            .store()
            .thread()
            .runs_of_message(&mid)
            .unwrap()
            .pop()
            .unwrap();
        let unread = store
            .store()
            .read(|r| attention::replies(r, WS, &user))
            .unwrap()
            .0;
        report.case(
            "core.stream.01_answer_error_after_partial_draft_explicit_error",
            &[
                ("single_model_call", gw.calls.load(Ordering::SeqCst) == 1),
                ("marked_error", replies[0].meta["error"] == json!(true)),
                ("le1_final_reply_per_intent", replies.len() == 1),
                (
                    "single_unread_on_commit",
                    unread.len() == 1 && run.reply_id.as_deref() == Some(replies[0].id.as_str()),
                ),
            ],
            &[],
        );
    }

    // 02: Research RunEnd::Failed with partial text must mark explicit error (not look like success!)
    {
        let k = replay_kit(true);
        let r = Arc::new(ScriptedResearcher {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: false,
            outcome_end: RunEnd::Failed,
            outcome_text: "读到一半的半截文字",
            outcome_offer: None,
            outcome_error: Some("stream_eof"),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么失败？", &[]).unwrap();
        r.wait_entered().await;
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let explicit_error = replies[0].meta["error"] == json!(true)
            || replies[0].text.contains("失败")
            || replies[0].text.contains("没查完");
        report.case(
            "core.stream.02_research_failed_with_partial_text_marks_error",
            &[
                ("stream_failed_explicit_error", explicit_error),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &["stream_failed_explicit_error"],
        );
    }

    // 03: Research RunEnd::Failed with empty text writes explicit failure message, 0 auto-retry
    {
        let k = replay_kit(true);
        let r = Arc::new(ScriptedResearcher {
            entered: Arc::new(Notify::new()),
            release: Arc::new(Notify::new()),
            released: std::sync::atomic::AtomicBool::new(false),
            seen: Mutex::new(vec![]),
            ignore_stop: false,
            outcome_end: RunEnd::Failed,
            outcome_text: "",
            outcome_offer: None,
            outcome_error: Some("adapter_eof"),
            in_flight: AtomicUsize::new(0),
            max_in_flight: AtomicUsize::new(0),
        });
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么报错？", &[]).unwrap();
        r.wait_entered().await;
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.stream.03_research_failed_empty_text_explicit_error",
            &[
                ("single_attempt", r.seen.lock().unwrap().len() == 1),
                ("explicit_failure_text", replies[0].text.contains("没查完")),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 04: Research RunEnd::Budget marks budget stop & single receipt, 0 auto-retry
    {
        let k = replay_kit(true);
        let r = ScriptedResearcher::new(RunEnd::Budget, "读了一部分");
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么超预算？", &[]).unwrap();
        r.wait_entered().await;
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.stream.04_research_budget_exhausted_partial_and_offer",
            &[
                ("single_attempt", r.seen.lock().unwrap().len() == 1),
                ("budget_note", replies[0].text.contains("没读完就停了")),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 05: Research RunEnd::Violated marks tool check stop, 0 auto-retry
    {
        let k = replay_kit(true);
        let r = ScriptedResearcher::new(RunEnd::Violated, "");
        k.core.set_researcher(r.clone());
        let mid = k.core.say(WS, "为什么工具被拦？", &[]).unwrap();
        r.wait_entered().await;
        r.open();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.stream.05_research_violated_tool_check_explicit_stop",
            &[
                ("single_attempt", r.seen.lock().unwrap().len() == 1),
                ("violated_note", replies[0].text.contains("工具检查时停止")),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 06..08: SingleModelGateway staged timeouts (Queue / FirstByte / Execution) -> <=1 call, 0 fallback
    for (idx, phase, name) in [
        ("06", TimeoutPhase::Queue, "queue"),
        ("07", TimeoutPhase::FirstByte, "first_byte"),
        ("08", TimeoutPhase::Execution, "execution"),
    ] {
        let one = ScriptedOneModel {
            calls: AtomicUsize::new(0),
            allow_tools: true,
            fail_with: Mutex::new(Some(OneShotError::Timeout(phase))),
        };
        let gw = SingleModelGateway {
            one,
            fast: vec!["m1".into(), "m2".into()],
            smart: vec!["m1".into(), "m2".into()],
            models: None,
        };
        let res = gw
            .call(
                "prompt",
                Tier::Smart,
                Duration::from_secs(5),
                &ThinkTools::None,
            )
            .await;
        let phase_ok =
            matches!(res, Err(CoreError::Gateway(GatewayError::Timeout(p))) if p == phase);
        let calls = gw.one.calls.load(Ordering::SeqCst);
        report.case(
            format!("core.stream.{idx}_gateway_timeout_{name}_single_attempt"),
            &[
                ("typed_timeout_phase", phase_ok),
                ("single_attempt_no_fallback", calls == 1),
            ],
            &[],
        );
    }

    // 09: SingleModelGateway startup error -> 1 attempt, 0 blanket fallback to 2nd model
    {
        let one = ScriptedOneModel {
            calls: AtomicUsize::new(0),
            allow_tools: true,
            fail_with: Mutex::new(Some(OneShotError::Failed("conn refused".into()))),
        };
        let gw = SingleModelGateway {
            one,
            fast: vec!["m1".into(), "m2".into()],
            smart: vec!["m1".into(), "m2".into()],
            models: None,
        };
        let res = gw
            .call(
                "prompt",
                Tier::Smart,
                Duration::from_secs(5),
                &ThinkTools::None,
            )
            .await;
        report.case(
            "core.stream.09_gateway_startup_error_no_blanket_fallback",
            &[
                (
                    "typed_startup_err",
                    matches!(res, Err(CoreError::Gateway(GatewayError::Startup(_)))),
                ),
                (
                    "single_attempt_no_fallback",
                    gw.one.calls.load(Ordering::SeqCst) == 1,
                ),
            ],
            &[],
        );
    }

    // 10: Router failure fallback never creates an item (even with explicit item wording)
    {
        let k = replay_kit(true);
        k.gw.set(|p, _| {
            if p.starts_with("You are the router") {
                Err(msg("router down"))
            } else {
                Ok(json!({"thought": "", "answer": "降级回答", "offer": null}).to_string())
            }
        });
        let mid = k.core.say(WS, "帮我建个事项：修按钮", &[]).unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.stream.10_router_failure_fallback_never_makes_item",
            &[
                ("zero_items_created", k.db().items(WS).unwrap().is_empty()),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }
}

// =========================================================================
// 7. Restart Before / After Commit & Crash Recovery (14 cases)
// =========================================================================

async fn run_restart(report: &mut Report) {
    // 01: Crash before claim (message + core_queue pending) -> resumes cleanly once
    {
        let k = replay_kit(false);
        let mid = k.db().message_add(WS, "me", "/汇报", None, None).unwrap();
        let job = json!({
            "mid": mid, "text": "/汇报", "files": [], "user": null,
            "mentions": [], "to": {"item": null, "reply_to": null},
            "conv": {"space": "steve", "research": true, "items": true, "owner_only": false, "start": true, "hands_on": false}
        });
        k.db()
            .store()
            .write(|w| core_queue::enqueue(w, WS, &mid, &job.to_string(), 100))
            .unwrap();
        let c = k.reopened();
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.restart.01_crash_before_claim_resumes_once",
            &[
                ("ready_no_failures", ready.failed == 0),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 02: Crash after claim before route/run -> posts 1 explicit rerun notice, 0 auto-execution
    {
        let k = replay_kit(false);
        let mid = k
            .db()
            .message_add(WS, "me", "刚领完就崩了", None, None)
            .unwrap();
        let c = k.reopened();
        c.thread_recover().unwrap();
        c.thread_ready().unwrap();
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.restart.02_crash_after_claim_before_route_explicit_rerun",
            &[
                (
                    "unconfirmed_start_zero_auto_replay",
                    router_calls(&k) == 0 && answer_calls(&k) == 0,
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
                (
                    "explicit_rerun_notice",
                    replies[0].meta["rerun"].as_str() == Some(mid.as_str()),
                ),
            ],
            &[],
        );
    }

    // 03: Crash during RUNNING main run (unconfirmed start / unknown outcome) -> must NOT auto-replay!
    {
        let k = replay_kit(false);
        let fr = FakeResearch::new(vec![Step::Text("自动重算出的第二份回复")]);
        let mid = k
            .db()
            .message_add(WS, "me", "为什么慢？", None, None)
            .unwrap();
        let run = k
            .db()
            .store()
            .write(|w| {
                let r = thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )?;
                thread_runs::patch(
                    w,
                    &r.id,
                    &RunPatch {
                        status: Some("running".into()),
                        started: Some(100),
                        ..Default::default()
                    },
                )?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .unwrap();
        let _ = run;
        let c = k.reopened();
        c.set_researcher(fr.clone());
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        if ready.resumed > 0 {
            wait_until("resumed finished", || !fr.seen.lock().unwrap().is_empty()).await;
            settle_replies(&k, WS, MAIN, &mid, 2).await;
        }
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.restart.03_crash_during_running_main_run_no_auto_replay",
            &[
                (
                    "unconfirmed_start_zero_auto_replay",
                    fr.seen.lock().unwrap().is_empty() && ready.resumed == 0,
                ),
                ("le1_final_reply_per_intent", replies.len() <= 1),
            ],
            &[
                "unconfirmed_start_zero_auto_replay",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 04: Crash TWICE during running main run -> bounded to <=1 final reply and 0 auto-replay
    {
        let k = replay_kit(false);
        let mid = k
            .db()
            .message_add(WS, "me", "两次崩溃为什么？", None, None)
            .unwrap();
        let _run = k
            .db()
            .store()
            .write(|w| {
                let r = thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )?;
                thread_runs::patch(
                    w,
                    &r.id,
                    &RunPatch {
                        status: Some("running".into()),
                        started: Some(100),
                        ..Default::default()
                    },
                )?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .unwrap();
        // Boot 1: auto-resumes into gated researcher and crashes again before reply
        let c1 = k.reopened();
        let r1 = ScriptedResearcher::new(RunEnd::Done, "never");
        c1.set_researcher(r1.clone());
        c1.thread_recover().unwrap();
        let ready1 = c1.thread_ready().unwrap();
        r1.wait_entered().await;
        // Boot 2: second crash while running
        let c2 = k.reopened();
        c2.thread_recover().unwrap();
        let _ready2 = c2.thread_ready().unwrap();
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.restart.04_crash_twice_during_running_main_run",
            &[
                ("unconfirmed_start_zero_auto_replay", ready1.resumed == 0),
                ("le1_final_reply_per_intent", replies.len() <= 1),
            ],
            &[
                "unconfirmed_start_zero_auto_replay",
                "le1_final_reply_per_intent",
            ],
        );
    }

    // 05: Crash during QUEUED (proven unstarted) main run -> must produce <=1 Steve reply (no extra resume_pending msg!)
    {
        let k = replay_kit(false);
        let fr = FakeResearch::new(vec![Step::Text("正常补答")]);
        let mid = k
            .db()
            .message_add(WS, "me", "排队未起为什么？", None, None)
            .unwrap();
        k.db()
            .store()
            .write(|w| {
                thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )
            })
            .unwrap();
        let c = k.reopened();
        c.set_researcher(fr.clone());
        c.thread_recover().unwrap();
        c.thread_ready().unwrap();
        wait_until("queued resumed", || !fr.seen.lock().unwrap().is_empty()).await;
        settle_replies(&k, WS, MAIN, &mid, 2).await;
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        report.case(
            "core.restart.05_crash_during_queued_main_run_single_reply",
            &[("le1_final_reply_per_intent", replies.len() <= 1)],
            &["le1_final_reply_per_intent"],
        );
    }

    // 06: Crash AFTER reply with run_receipt persisted, BEFORE thread_runs patched to done -> 0 recompute
    {
        let k = replay_kit(false);
        let fr = FakeResearch::new(vec![Step::Text("绝不能重算")]);
        let mid = k
            .db()
            .message_add(WS, "me", "已落盘回复为什么？", None, None)
            .unwrap();
        let run = k
            .db()
            .store()
            .write(|w| {
                let r = thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )?;
                thread_runs::patch(
                    w,
                    &r.id,
                    &RunPatch {
                        status: Some("running".into()),
                        started: Some(100),
                        ..Default::default()
                    },
                )?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .unwrap();
        let _reply_id = k
            .db()
            .message_add(
                WS,
                "steve",
                "已提交的答案",
                None,
                Some(&json!({"origin": mid, "run": run.id, "run_receipt": true})),
            )
            .unwrap();
        let c = k.reopened();
        c.set_researcher(fr.clone());
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        let replies = steve_replies_for(&k, WS, MAIN, &mid);
        let final_status = k
            .db()
            .store()
            .thread()
            .run(&run.id)
            .unwrap()
            .unwrap()
            .status;
        report.case(
            "core.restart.06_crash_after_reply_receipt_before_run_done",
            &[
                (
                    "committed_reply_zero_recompute",
                    fr.seen.lock().unwrap().is_empty() && ready.resumed == 0,
                ),
                ("reconciled_done", final_status == "done"),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 07: Committed done reply + double restart -> 0 recompute, 0 extra reply, 0 extra unread
    {
        let k = replay_kit(false);
        let user = k
            .db()
            .store()
            .users()
            .create("local:r07", "U", None, "owner")
            .unwrap();
        let fr = FakeResearch::new(vec![Step::Text("已提交答案")]);
        k.core.set_researcher(fr.clone());
        let mid = k
            .core
            .say_as(WS, Some(&user), "稳定提交为什么？", &[], &[])
            .unwrap();
        settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let unread_before = k
            .db()
            .store()
            .read(|r| attention::replies(r, WS, &user))
            .unwrap()
            .0
            .len();
        for _ in 0..2 {
            let c = k.reopened();
            c.set_researcher(fr.clone());
            c.thread_recover().unwrap();
            c.thread_ready().unwrap();
        }
        let unread_after = k
            .db()
            .store()
            .read(|r| attention::replies(r, WS, &user))
            .unwrap()
            .0
            .len();
        report.case(
            "core.restart.07_committed_done_double_restart_zero_recompute",
            &[
                (
                    "committed_reply_zero_recompute",
                    fr.seen.lock().unwrap().len() == 1,
                ),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &mid).len() == 1,
                ),
                (
                    "duplicate_reply_zero_new_unread",
                    unread_before == 1 && unread_after == 1,
                ),
            ],
            &[],
        );
    }

    // 08: Crash during running guest run -> 0 auto-replay, 0 host.said callback
    {
        let k = replay_kit(false);
        let fr = FakeResearch::new(vec![Step::Text("不应重算")]);
        let host = ReplayHost::new();
        let mid = k
            .db()
            .message_add_in(WS, "chan:sp_rc", "me", "访客崩了", None, &json!({}))
            .unwrap();
        k.db()
            .store()
            .write(|w| {
                let r = thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )?;
                thread_runs::patch(
                    w,
                    &r.id,
                    &RunPatch {
                        status: Some("running".into()),
                        started: Some(100),
                        ..Default::default()
                    },
                )?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .unwrap();
        let c = k.reopened();
        c.set_researcher(fr.clone());
        c.set_conv_host(host.clone());
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        let replies = steve_replies_for(&k, WS, "chan:sp_rc", &mid);
        report.case(
            "core.restart.08_crash_during_running_guest_run_no_auto_replay",
            &[
                (
                    "unconfirmed_start_zero_auto_replay",
                    fr.seen.lock().unwrap().is_empty() && ready.resumed == 0,
                ),
                (
                    "zero_external_callback",
                    host.said.lock().unwrap().is_empty(),
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 09: Crash with STEVE_NO_RESUME=1 -> 0 auto-replay, 1 explicit rerun notice
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("不应重算")]);
        let mid = k
            .db()
            .message_add(WS, "me", "禁恢复测试", None, None)
            .unwrap();
        k.db()
            .store()
            .write(|w| {
                let r = thread_runs::add(
                    w,
                    &NewThreadRun {
                        workspace_id: WS.into(),
                        message_id: mid.clone(),
                        ..Default::default()
                    },
                )?;
                thread_runs::patch(
                    w,
                    &r.id,
                    &RunPatch {
                        status: Some("running".into()),
                        started: Some(100),
                        ..Default::default()
                    },
                )?;
                Ok::<_, steve_store::StoreError>(r)
            })
            .unwrap();
        let c = k.reopened();
        c.set_researcher(fr.clone());
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        report.case(
            "core.restart.09_crash_with_no_resume_flag_no_auto_replay",
            &[
                (
                    "unconfirmed_start_zero_auto_replay",
                    fr.seen.lock().unwrap().is_empty() && ready.resumed == 0,
                ),
                (
                    "le1_final_reply_per_intent",
                    steve_replies_for(&k, WS, MAIN, &mid).len() == 1,
                ),
            ],
            &[],
        );
    }

    // 10: Corrupt JSON in core_queue fails closed and posts rerun notice
    {
        let k = replay_kit(false);
        let mid = k
            .db()
            .message_add(WS, "me", "坏队列数据", None, None)
            .unwrap();
        k.db()
            .store()
            .write(|w| core_queue::enqueue(w, WS, &mid, "{corrupt json", 100))
            .unwrap();
        let c = k.reopened();
        c.thread_recover().unwrap();
        let ready = c.thread_ready().unwrap();
        report.case(
            "core.restart.10_corrupt_core_queue_json_fails_closed",
            &[
                ("failed_counted", ready.failed == 1),
                ("unconfirmed_start_zero_auto_replay", router_calls(&k) == 0),
                (
                    "explicit_rerun_posted",
                    steve_replies_for(&k, WS, MAIN, &mid).len() == 1,
                ),
            ],
            &[],
        );
    }

    // 11: Mismatched mid in core_queue payload fails closed with 0 cross-message routing
    {
        let k = replay_kit(false);
        let m1 = k.db().message_add(WS, "me", "消息1", None, None).unwrap();
        let m2 = k.db().message_add(WS, "me", "/汇报", None, None).unwrap();
        let mismatched = json!({
            "mid": m2, "text": "/汇报", "files": [], "user": null,
            "mentions": [], "to": {"item": null, "reply_to": null},
            "conv": {"space": "steve", "research": true, "items": true, "owner_only": false, "start": true, "hands_on": false}
        });
        k.db()
            .store()
            .write(|w| core_queue::enqueue(w, WS, &m1, &mismatched.to_string(), 100))
            .unwrap();
        let c = k.reopened();
        let ready = c.thread_ready().unwrap();
        tokio::time::sleep(Duration::from_millis(40)).await;
        report.case(
            "core.restart.11_mismatched_core_queue_mid_fails_closed",
            &[
                ("failed_counted", ready.failed == 1),
                (
                    "zero_cross_execution",
                    steve_replies_for(&k, WS, MAIN, &m2).is_empty(),
                ),
            ],
            &[],
        );
    }

    // 12: Restart preserves attention read watermark (0 unread re-added)
    {
        let k = replay_kit(false);
        let user = k
            .db()
            .store()
            .users()
            .create("local:r12", "U", None, "owner")
            .unwrap();
        let fr = FakeResearch::new(vec![Step::Text("已读答案")]);
        k.core.set_researcher(fr);
        let mid = k
            .core
            .say_as(WS, Some(&user), "水印测试为什么？", &[], &[])
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let snap = k
            .db()
            .store()
            .write(|w| {
                let (entries, upper) = attention::replies(w, WS, &user)?;
                let cat = attention::page(
                    w, WS, &user, "replies", entries, upper, None, None, 50, 1_000,
                )?;
                Ok::<_, steve_store::StoreError>(cat.snapshot.unwrap())
            })
            .unwrap();
        k.db()
            .store()
            .write(|w| attention::read_replies(w, WS, &user, &snap, &replies[0].id, 1_001))
            .unwrap();
        let c = k.reopened();
        c.thread_recover().unwrap();
        c.thread_ready().unwrap();
        let unread = k
            .db()
            .store()
            .read(|r| attention::replies(r, WS, &user))
            .unwrap()
            .0;
        report.case(
            "core.restart.12_restart_preserves_attention_read_watermark",
            &[("duplicate_reply_zero_new_unread", unread.is_empty())],
            &[],
        );
    }

    // 13: Duplicate reply committed after stale/restart resume adds extra unread for same origin!
    {
        let k = replay_kit(false);
        let user = k
            .db()
            .store()
            .users()
            .create("local:r13", "U", None, "owner")
            .unwrap();
        let r1 = ScriptedResearcher::new(RunEnd::Done, "第一次答");
        k.core.set_researcher(r1.clone());
        let mid = k
            .core
            .say_as(WS, Some(&user), "未读重复测试为什么？", &[], &[])
            .unwrap();
        r1.wait_entered().await;
        let c2 = k.reopened();
        let r2 = ScriptedResearcher::new(RunEnd::Done, "第二次答");
        c2.set_researcher(r2.clone());
        c2.thread_recover().unwrap();
        c2.thread_ready().unwrap();
        r2.wait_entered().await;
        r2.open();
        wait_until("second committed", || {
            steve_replies_for(&k, WS, MAIN, &mid)
                .iter()
                .any(|m| m.text.contains("第二次答"))
        })
        .await;
        r1.open();
        wait_until("first exited", || r1.in_flight.load(Ordering::SeqCst) == 0).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        let unread = k
            .db()
            .store()
            .read(|r| attention::replies(r, WS, &user))
            .unwrap()
            .0;
        report.case(
            "core.restart.13_duplicate_resumed_reply_adds_zero_extra_unread",
            &[("duplicate_reply_zero_new_unread", unread.len() <= 1)],
            &["duplicate_reply_zero_new_unread"],
        );
    }

    // 14: Boot thread_recover must NOT post unsolicited hardcoded ST-20 postback messages!
    {
        let k = replay_kit(true);
        let item_id = k
            .core
            .create_item(crate::CreateItem {
                ws: WS.into(),
                goal: "问题事项".into(),
                title: "问题".into(),
                source: Some(("对话".into(), "chat".into())),
                propose: true,
                ..Default::default()
            })
            .unwrap()
            .id;
        let _u = k.db().message_add(WS, "me", "问个事", None, None).unwrap();
        let _a = k
            .db()
            .message_add(WS, "steve", "交给它了", Some(&item_id), None)
            .unwrap();
        k.db()
            .decision_create(crate::repo::NewDecision {
                workspace_id: WS.into(),
                kind: "accept".into(),
                subject_type: "item".into(),
                subject_id: item_id.clone(),
                item_id: Some(item_id.clone()),
                question: "验收".into(),
                draft: Some("交付摘要内容".into()),
                options: Some(vec!["pass".into()]),
                requested_by: "dev".into(),
                ..Default::default()
            })
            .unwrap();
        let posted = k.core.postback(&[item_id.as_str()]).unwrap();
        report.case(
            "core.restart.14_boot_postback_no_unsolicited_message",
            &[("no_boot_postback_mutation", posted.is_empty())],
            &["no_boot_postback_mutation"],
        );
    }
}

// =========================================================================
// 8. Parallel Spaces & Multi-Workspace Concurrency (10 cases)
// =========================================================================

async fn run_spaces(report: &mut Report) {
    // 01: Guest space answers in parallel without waiting behind slow MAIN research
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        let conv = Conv {
            space: "chan:sp_par1".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let g = k.core.say_in(WS, conv, "为什么呢？", &json!({})).unwrap();
        // Guest must finish WHILE MAIN is still blocked on gate!
        let g_replies = settle_replies(&k, WS, "chan:sp_par1", &g, 1).await;
        let main_still_unsettled = steve_replies_for(&k, WS, MAIN, &first).is_empty();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.spaces.01_guest_space_not_blocked_by_main_slow_research",
            &[(
                "guest_answered_while_main_blocked",
                g_replies.len() == 1 && main_still_unsettled,
            )],
            &[],
        );
    }

    // 02: Two guest spaces run concurrently
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        let entered = Arc::new(Notify::new());
        let release = Arc::new(Notify::new());
        *host.block_answer.lock().unwrap() = Some((entered.clone(), release.clone()));
        k.core.set_conv_host(host.clone());
        let c1 = Conv {
            space: "chan:sp_a".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let c2 = Conv {
            space: "chan:sp_b".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let m1 = k.core.say_in(WS, c1, "为什么A？", &json!({})).unwrap();
        entered.notified().await;
        // Unblock only future calls so sp_b finishes while sp_a is still blocked
        *host.block_answer.lock().unwrap() = None;
        let m2 = k.core.say_in(WS, c2, "为什么B？", &json!({})).unwrap();
        let r2 = settle_replies(&k, WS, "chan:sp_b", &m2, 1).await;
        let a_still_blocked = steve_replies_for(&k, WS, "chan:sp_a", &m1).is_empty();
        release.notify_waiters();
        let r1 = settle_replies(&k, WS, "chan:sp_a", &m1, 1).await;
        report.case(
            "core.spaces.02_two_guest_spaces_run_concurrently",
            &[
                (
                    "sp_b_finished_during_sp_a",
                    r2.len() == 1 && a_still_blocked,
                ),
                ("sp_a_finished_once", r1.len() == 1),
            ],
            &[],
        );
    }

    // 03: Two workspaces run concurrently with zero cross-workspace leakage
    {
        let k = replay_kit(true);
        let ws2 = second_ws(&k);
        let (fr, first) = hold_main_turn(&k).await;
        let m2 = k.core.say(&ws2, "/汇报", &[]).unwrap();
        let r2 = settle_replies(&k, &ws2, MAIN, &m2, 1).await;
        let ws1_still_blocked = steve_replies_for(&k, WS, MAIN, &first).is_empty();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        wait_idle_main(&k, WS).await;
        let ws2_msgs = k.db().messages_in(&ws2, MAIN, 200).unwrap();
        let isolated = ws2_msgs.iter().all(|m| m.workspace_id == ws2);
        report.case(
            "core.spaces.03_two_workspaces_run_concurrently_isolated",
            &[
                ("ws2_not_blocked_by_ws1", r2.len() == 1 && ws1_still_blocked),
                ("zero_cross_ws_leak", isolated),
            ],
            &[],
        );
    }

    // 04: Same-space direct command (/汇报) must NOT overtake unsettled space head!
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        let second = k.core.say(WS, "/汇报", &[]).unwrap();
        // Give direct_lane a moment to overtake if it bypasses the space head
        tokio::time::sleep(Duration::from_millis(60)).await;
        let overtook = !steve_replies_for(&k, WS, MAIN, &second).is_empty()
            && steve_replies_for(&k, WS, MAIN, &first).is_empty();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &second, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.spaces.04_same_space_direct_does_not_overtake_head",
            &[("same_space_serial_order", !overtook)],
            &["same_space_serial_order"],
        );
    }

    // 05: Same-space ahead route (high-confidence item) must NOT overtake unsettled space head!
    {
        let k = replay_kit(true);
        let (fr, first) = hold_main_turn(&k).await;
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "after": false, "signals": ["change"], "title": "抢跑", "goal": "抢跑目标"}),
            "ok",
        );
        let second = k.core.say(WS, "修登录页样式", &[]).unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        let overtook = !steve_replies_for(&k, WS, MAIN, &second).is_empty()
            && steve_replies_for(&k, WS, MAIN, &first).is_empty();
        fr.open(1);
        settle_replies(&k, WS, MAIN, &first, 1).await;
        settle_replies(&k, WS, MAIN, &second, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.spaces.05_same_space_ahead_does_not_overtake_head",
            &[("same_space_serial_order", !overtook)],
            &["same_space_serial_order"],
        );
    }

    // 06: Queue full in MAIN does not block guest space
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("ok"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        let m1 = k.core.say(WS, "为什么1？", &[]).unwrap();
        wait_until("m1 running", || !fr.seen.lock().unwrap().is_empty()).await;
        let conv = Conv {
            space: "chan:sp_free".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let g = k.core.say_in(WS, conv, "为什么访客？", &json!({})).unwrap();
        let g_replies = settle_replies(&k, WS, "chan:sp_free", &g, 1).await;
        fr.open(1);
        settle_replies(&k, WS, MAIN, &m1, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.spaces.06_main_busy_does_not_block_guest_space",
            &[("guest_answered", g_replies.len() == 1)],
            &[],
        );
    }

    // 07: Guest research completion must NOT break MAIN's per-space resume_for session!
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("研究结果")]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        // MAIN completes a research run with session cs_1
        let m1 = k.core.say(WS, "主线问题为什么？", &[]).unwrap();
        settle_replies(&k, WS, MAIN, &m1, 1).await;
        wait_idle_main(&k, WS).await;
        // Now guest space completes a research run (overwriting workspace-wide run_last_done!)
        let conv = Conv {
            space: "chan:sp_res".into(),
            research: true,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let g1 = k.core.say_in(WS, conv, "访客研究一下", &json!({})).unwrap();
        settle_replies(&k, WS, "chan:sp_res", &g1, 1).await;
        // Now MAIN asks a follow-up right away -> resume_for should still find MAIN's run!
        let m2 = k.core.say(WS, "继续追问为什么？", &[]).unwrap();
        settle_replies(&k, WS, MAIN, &m2, 1).await;
        wait_idle_main(&k, WS).await;
        let m2_resumed = fr
            .seen
            .lock()
            .unwrap()
            .iter()
            .find(|r| r.mid == m2)
            .is_some_and(|r| r.resume.is_some());
        report.case(
            "core.spaces.07_guest_completion_preserves_main_resume_session",
            &[("per_space_resume_isolation", m2_resumed)],
            &["per_space_resume_isolation"],
        );
    }

    // 08: Guest "好，改吧" does NOT confirm MAIN's open offer
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        let origin = k.db().message_add(WS, "me", "改吗", None, None).unwrap();
        let _offer_msg = k
            .db()
            .message_add(
                WS,
                "steve",
                "要改吗？",
                None,
                Some(&json!({"origin": origin, "offer": {"title": "主线提议", "goal": "主线目标", "state": "open"}})),
            )
            .unwrap();
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["explicit"], "title": "访客", "goal": "访客"}),
            "ok",
        );
        let conv = Conv {
            space: "chan:sp_offer".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let g = k.core.say_in(WS, conv, "好，改吧", &json!({})).unwrap();
        settle_replies(&k, WS, "chan:sp_offer", &g, 1).await;
        let main_offer_still_open = k
            .db()
            .messages_in(WS, MAIN, 200)
            .unwrap()
            .iter()
            .any(|m| m.meta["offer"]["state"] == "open");
        report.case(
            "core.spaces.08_guest_confirm_does_not_take_main_offer",
            &[
                ("main_offer_untouched", main_offer_still_open),
                ("zero_items_created", k.db().items(WS).unwrap().is_empty()),
            ],
            &[],
        );
    }

    // 09: stop(ws) stops ONLY MAIN, not an active guest space run
    {
        let k = replay_kit(true);
        let fr = FakeResearch::new(vec![Step::Text("结果"), Step::Gate]);
        k.core.set_researcher(fr.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        let main_m = k.core.say(WS, "主线为什么？", &[]).unwrap();
        let conv = Conv {
            space: "chan:sp_keep".into(),
            research: true,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let guest_m = k.core.say_in(WS, conv, "访客深度查", &json!({})).unwrap();
        wait_until("both running", || fr.seen.lock().unwrap().len() >= 2).await;
        k.core.stop(WS).unwrap();
        settle_replies(&k, WS, MAIN, &main_m, 1).await;
        fr.open(1);
        let guest_replies = settle_replies(&k, WS, "chan:sp_keep", &guest_m, 1).await;
        let guest_run = k
            .db()
            .store()
            .thread()
            .runs_of_message(&guest_m)
            .unwrap()
            .pop()
            .unwrap();
        report.case(
            "core.spaces.09_stop_main_does_not_stop_guest_space",
            &[(
                "guest_completed_done",
                guest_run.status == "done" && guest_replies.len() == 1,
            )],
            &[],
        );
    }

    // 10: Many parallel spaces should respect a global concurrency cap (not unbounded spawn)
    {
        let k = replay_kit(true);
        let r = ScriptedResearcher::new(RunEnd::Done, "并发回答");
        k.core.set_researcher(r.clone());
        let host = ReplayHost::new();
        k.core.set_conv_host(host);
        set_scripted_gateway(&k, json!({"kind": "research", "confidence": 0.95}), "ok");
        let mut mids = vec![];
        for i in 0..6 {
            let conv = Conv {
                space: format!("chan:sp_cap_{i}"),
                research: true,
                items: false,
                owner_only: false,
                start: false,
                hands_on: false,
            };
            let m = k
                .core
                .say_in(WS, conv, "帮我深入查一下系统数据和当前服务配置", &json!({}))
                .unwrap();
            mids.push((format!("chan:sp_cap_{i}"), m));
        }
        wait_until("parallel space entered", || {
            r.in_flight.load(Ordering::SeqCst) >= 1
        })
        .await;
        // Let concurrent spaces saturate up to any concurrency cap (without hanging if capped <= 4)
        let sat_deadline = tokio::time::Instant::now() + Duration::from_millis(120);
        while r.in_flight.load(Ordering::SeqCst) < 6 && tokio::time::Instant::now() < sat_deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let peak = r.max_in_flight.load(Ordering::SeqCst);
        r.open();
        for (sp, m) in &mids {
            settle_replies(&k, WS, sp, m, 1).await;
        }
        // Global budget cap expected <= 4 in W3.4; currently unbounded (all 6 enter at once!)
        report.case(
            "core.spaces.10_parallel_spaces_respect_global_concurrency_budget",
            &[("parallel_spaces_global_budget", peak <= 4)],
            &["parallel_spaces_global_budget"],
        );
    }
}

// =========================================================================
// 9. Deterministic Routing, Tool Preflight & Single Generation (12 cases)
// =========================================================================

async fn run_routing(report: &mut Report) {
    // 01: /汇报 -> 0 router calls, 0 answer model calls
    {
        let k = replay_kit(true);
        let mid = k.core.say(WS, "/汇报", &[]).unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.01_digest_command_zero_router_zero_llm",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                ("zero_llm_calls", answer_calls(&k) == 0),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 02: Explicit SayTo { item } -> 0 router calls, 1 supplement
    {
        let k = replay_kit(true);
        let item = k.item("run", "执行", None);
        let _s = k.session(&item, "idle");
        let mid = k
            .core
            .say_to(
                WS,
                None,
                "顺便把测试也补上",
                &[],
                &[],
                SayTo {
                    item: Some(item.clone()),
                    reply_to: None,
                },
            )
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.02_explicit_item_target_zero_router_calls",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                (
                    "supplement_delivered",
                    k.db().timeline(&item).unwrap().len() == 1,
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 03: Explicit SayTo { item } + progress query -> 0 router calls, 0 answer LLM calls
    {
        let k = replay_kit(true);
        let item = k.item("run", "执行", None);
        let mid = k
            .core
            .say_to(
                WS,
                None,
                "进展怎么样了？",
                &[],
                &[],
                SayTo {
                    item: Some(item),
                    reply_to: None,
                },
            )
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.03_explicit_item_progress_direct_readback_zero_llm",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                ("zero_llm_calls", answer_calls(&k) == 0),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 04: Explicit SayTo { reply_to: item_announce } -> 0 router calls
    {
        let k = replay_kit(true);
        let item = k.item("run", "执行", None);
        let _s = k.session(&item, "idle");
        let announce = k
            .db()
            .message_add(WS, "steve", "已建项", Some(&item), None)
            .unwrap();
        let mid = k
            .core
            .say_to(
                WS,
                None,
                "再加一条要求",
                &[],
                &[],
                SayTo {
                    item: None,
                    reply_to: Some(announce),
                },
            )
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.04_reply_to_item_message_zero_router_calls",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                (
                    "supplement_delivered",
                    k.db().timeline(&item).unwrap().len() == 1,
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 05: Plain question -> 0 router calls, 1 single generation
    {
        let k = replay_kit(true);
        set_scripted_gateway(&k, json!({"kind": "answer", "confidence": 0.9}), "直接回答");
        let mid = k.core.say(WS, "检索是怎么打分的？", &[]).unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.05_plain_question_zero_router_single_generation",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                ("obvious_answer_single_generation", answer_calls(&k) == 1),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 06: Guest short plain question -> 0 router calls, 1 single generation
    {
        let k = replay_kit(true);
        let host = ReplayHost::new();
        k.core.set_conv_host(host.clone());
        let conv = Conv {
            space: "chan:sp_pq".into(),
            research: false,
            items: false,
            owner_only: false,
            start: false,
            hands_on: false,
        };
        let mid = k
            .core
            .say_in(WS, conv, "为什么周四的会议推迟了？", &json!({}))
            .unwrap();
        let replies = settle_replies(&k, WS, "chan:sp_pq", &mid, 1).await;
        report.case(
            "core.routing.06_guest_short_plain_question_zero_router_single_gen",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                (
                    "obvious_answer_single_generation",
                    host.answered.lock().unwrap().len() == 1,
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &[],
        );
    }

    // 07: Explicit @Bot mention must make 0 router model calls!
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["explicit"], "title": "修登录页", "goal": "修登录页"}),
            "ok",
        );
        let dev_name = k.db().bot("dev").unwrap().unwrap().name;
        let at_handle = dev_name.split_whitespace().collect::<String>();
        let mid = k
            .core
            .say(WS, &format!("@{at_handle} 修一下登录页"), &[])
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let items = k.db().items(WS).unwrap();
        report.case(
            "core.routing.07_explicit_at_bot_zero_router_calls",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                (
                    "assigned_to_named_bot",
                    items.len() == 1 && items[0].bot_id.as_deref() == Some("dev"),
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &["explicit_route_zero_router_calls"],
        );
    }

    // 08: Explicit item creation phrase ("帮我建个事项：...") must make 0 router model calls!
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "item", "confidence": 0.95, "signals": ["explicit"], "title": "改按钮", "goal": "把登录按钮改成蓝色"}),
            "ok",
        );
        let mid = k
            .core
            .say(WS, "帮我建个事项：把登录按钮改成蓝色", &[])
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let items = k.db().items(WS).unwrap();
        report.case(
            "core.routing.08_explicit_item_phrase_zero_router_calls",
            &[
                ("explicit_route_zero_router_calls", router_calls(&k) == 0),
                ("le1_dispatch_per_intent", items.len() == 1),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &["explicit_route_zero_router_calls"],
        );
    }

    // 09: Obvious conversational reply ("谢谢") should make 1 generation (not router + answer = 2!)
    {
        let k = replay_kit(true);
        set_scripted_gateway(
            &k,
            json!({"kind": "answer", "confidence": 0.95}),
            "不客气！",
        );
        let mid = k.core.say(WS, "谢谢", &[]).unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        let total_llm = router_calls(&k) + answer_calls(&k);
        report.case(
            "core.routing.09_obvious_greeting_single_generation",
            &[
                ("obvious_answer_single_generation", total_llm == 1),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &["obvious_answer_single_generation"],
        );
    }

    // 10: Tool-needing question in hands_on conv must preflight (0 probe calls to tool-less model!)
    {
        let k = replay_kit(true);
        let probed_without_tools = Arc::new(AtomicBool::new(false));
        let pflag = probed_without_tools.clone();
        k.gw.set(move |p, _| {
            if p.starts_with("You are the router") {
                Ok(json!({"kind": "answer", "confidence": 0.9}).to_string())
            } else {
                // Tool-less answer model was probed first and hit NeedsTools before hand_off!
                pflag.store(true, Ordering::SeqCst);
                Err(GatewayError::NeedsTools.into())
            }
        });
        let conv = Conv {
            space: MAIN.into(),
            research: false,
            items: true,
            owner_only: false,
            start: true,
            hands_on: true,
        };
        let mid = k
            .core
            .say_in(WS, conv, "帮我看下磁盘还剩多少", &json!({}))
            .unwrap();
        let replies = settle_replies(&k, WS, MAIN, &mid, 1).await;
        wait_idle_main(&k, WS).await;
        report.case(
            "core.routing.10_hands_on_needs_tools_preflight_zero_probe",
            &[
                (
                    "tool_preflight_zero_probe_calls",
                    !probed_without_tools.load(Ordering::SeqCst),
                ),
                (
                    "le1_dispatch_per_intent",
                    k.db().items(WS).unwrap().len() == 1,
                ),
                ("le1_final_reply_per_intent", replies.len() == 1),
            ],
            &["tool_preflight_zero_probe_calls"],
        );
    }

    // 11: CallSpec unauthorized tools -> ToolDenied, 0 model attempts
    {
        let req_tools = ThinkTools::Read(vec![Readable {
            file_id: "fil_secret".into(),
            rel_path: ".steve/attachments/fil_secret-secret.txt".into(),
        }]);
        let scope = ThinkTools::None;
        let spec = CallSpec {
            purpose: "chat",
            required_tools: &req_tools,
            auth_scope: &scope,
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        };
        let denied = matches!(
            spec.authorize(),
            Err(CoreError::Gateway(GatewayError::ToolDenied))
        );
        let mismatched = CallSpec {
            purpose: "chat",
            required_tools: &req_tools,
            auth_scope: &ThinkTools::Read(vec![Readable {
                file_id: "fil_other".into(),
                rel_path: ".steve/attachments/fil_other-other.txt".into(),
            }]),
            deadline: tokio::time::Instant::now() + Duration::from_secs(5),
        };
        let denied2 = matches!(
            mismatched.authorize(),
            Err(CoreError::Gateway(GatewayError::ToolDenied))
        );
        report.case(
            "core.routing.11_gateway_callspec_unauthorized_tools_denied",
            &[("unauthorized_tool_zero_attempt", denied && denied2)],
            &[],
        );
    }

    // 12: SingleModelGateway preflight missing capability -> MissingCapability, 0 model probe calls
    {
        let one = ScriptedOneModel {
            calls: AtomicUsize::new(0),
            allow_tools: false,
            fail_with: Mutex::new(None),
        };
        let gw = SingleModelGateway {
            one,
            fast: vec!["m1".into()],
            smart: vec!["m1".into()],
            models: None,
        };
        let tools = ThinkTools::Read(vec![Readable {
            file_id: "fil_doc".into(),
            rel_path: ".steve/attachments/fil_doc-doc.txt".into(),
        }]);
        let res = gw
            .call("read doc", Tier::Smart, Duration::from_secs(5), &tools)
            .await;
        report.case(
            "core.routing.12_gateway_preflight_missing_capability_zero_probe",
            &[
                (
                    "missing_capability_err",
                    matches!(
                        res,
                        Err(CoreError::Gateway(GatewayError::MissingCapability))
                    ),
                ),
                (
                    "tool_preflight_zero_probe_calls",
                    gw.one.calls.load(Ordering::SeqCst) == 0,
                ),
            ],
            &[],
        );
    }
}
